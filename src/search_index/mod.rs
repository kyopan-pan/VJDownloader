mod comments;
mod db;
mod normalize;
mod query;
mod scanner;
pub mod ui;
mod watcher;
mod writer;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use comments::{BackfillMessage, comment_backfill_loop};
use db::{apply_migrations, open_connection};
use normalize::{escape_like_pattern, normalize_query, normalize_root_path, path_to_key};
use query::{QueryPattern, run_search_query};
use scanner::scan_root;
use watcher::watcher_loop;
use writer::writer_loop;

const DB_SCHEMA_VERSION: i32 = 3;
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(700);
const UPSERT_BATCH_SIZE: usize = 256;
// 応答しない ffprobe でスキャンが止まらないようにする上限。moov アトムの読み取りは
// 低速なHDDでも数秒で終わるため、これを超えるものは異常とみなして打ち切る。
const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);
// フェーズ2で1度に取り出すコメント取得待ちの件数。
const COMMENT_BATCH_SIZE: usize = 256;
// フェーズ2で同時に走らせる ffprobe の数の上限。ffprobe はプロセス起動と
// ファイル読み取りの待ちが大半なので並列化が効くが、HDDのランダムシークが
// 律速になるため増やしすぎても頭打ちになる。
const MAX_COMMENT_WORKERS: usize = 4;
const MAX_SEARCH_LIMIT: usize = 1_000;

pub type EngineResult<T> = Result<T, String>;

#[derive(Clone, Debug)]
pub enum IndexEvent {
    UpdateStarted {
        target: IndexEventTarget,
    },
    UpdateFinished {
        target: IndexEventTarget,
        result: EngineResult<()>,
    },
}

// フルスキャンをどのルートに対して起こすか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanTrigger {
    // 今回新しく追加されたルートだけを走査する。
    NewRootsOnly,
    // 有効なルートすべてを走査する。
    AllRoots,
}

// インデックス作成の進捗。UI が毎フレーム読み取れるよう、値だけの複製可能な型にする。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexProgress {
    // フェーズ1（ファイル名インデックス）を実行中のルート数。
    pub scanning_roots: usize,
    // フェーズ1で積み上げたファイル数。総数は走査し終えるまで分からないため件数のみ。
    pub scanned_files: u64,
    // フェーズ2（コメント補完）が動いているか。
    pub comment_running: bool,
    // フェーズ2で処理を終えた件数と、残っている件数。
    pub comment_done: u64,
    pub comment_pending: u64,
}

impl IndexProgress {
    // 進捗ウィンドウを出すべき状態かどうか。
    pub fn is_active(&self) -> bool {
        self.scanning_roots > 0 || self.comment_running
    }

    // フェーズ2の進捗率。総数が分からないうちは None。
    pub fn comment_ratio(&self) -> Option<f32> {
        let total = self.comment_done + self.comment_pending;
        if total == 0 {
            return None;
        }
        Some(self.comment_done as f32 / total as f32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexEventTarget {
    Main,
    Settings,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum SearchSort {
    #[default]
    ModifiedDesc,
    NameAsc,
}

#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub query: String,
    pub root_id: Option<i64>,
    pub root_path: Option<String>,
    pub parent_dir: Option<String>,
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
    pub size_min: Option<i64>,
    pub size_max: Option<i64>,
    pub limit: usize,
    pub sort: SearchSort,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            root_id: None,
            root_path: None,
            parent_dir: None,
            modified_after: None,
            modified_before: None,
            size_min: None,
            size_max: None,
            limit: 100,
            sort: SearchSort::ModifiedDesc,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct SearchHit {
    pub path: String,
    pub file_name: String,
    pub size_bytes: i64,
    pub modified_time: i64,
    pub root_id: i64,
    pub parent_dir: String,
}

#[derive(Clone, Debug)]
pub struct RootEntry {
    pub root_id: i64,
    pub root_path: String,
    pub is_enabled: bool,
    #[allow(dead_code)]
    pub last_scan_time: Option<i64>,
}

#[derive(Clone)]
pub struct SearchEngine {
    inner: Arc<EngineInner>,
}

struct EngineInner {
    db_path: PathBuf,
    write_tx: Sender<WriteCommand>,
    watcher_tx: Sender<WatcherMessage>,
    scan_context: ScanContext,
    index_event_txs: Mutex<Vec<Sender<IndexEvent>>>,
}

#[derive(Debug)]
enum WriteCommand {
    AddOrEnableRoot {
        root_path: String,
        resp: Sender<EngineResult<i64>>,
    },
    RemoveRoot {
        root_id: i64,
        resp: Sender<EngineResult<()>>,
    },
    UpsertFiles {
        files: Vec<FileRecord>,
    },
    // 先行する書き込みがコミットされた後にフェーズ2へ Kick を送らせる。
    // 呼び出し側から直接 Kick すると、まだコミットされていない取得待ちを
    // 見落として0件で終わり、次の Kick も起きないまま取り残される。
    NotifyCommentPending,
    UpsertComments {
        comments: Vec<CommentRecord>,
        // 反映を待ってから次のバッチを引くための応答口。待たずに再検索すると
        // まだコミットされていない同じ行を引き当てて延々と処理し続けてしまう。
        resp: Sender<EngineResult<()>>,
    },
    DeletePaths {
        paths: Vec<String>,
    },
    DeleteByPrefixes {
        prefixes: Vec<String>,
    },
    FinalizeScan {
        root_id: i64,
        marker: i64,
        finished_at: i64,
        resp: Sender<EngineResult<()>>,
    },
    Shutdown,
}

// フェーズ1（ファイル名インデックス）で書き込むレコード。コメントは持たない。
#[derive(Clone, Debug)]
struct FileRecord {
    path: String,
    root_id: i64,
    file_name: String,
    file_name_norm: String,
    parent_dir: String,
    size_bytes: i64,
    modified_time: i64,
    created_time: Option<i64>,
    last_indexed_time: i64,
}

// フェーズ2（コメント補完）で書き込むレコード。
#[derive(Clone, Debug)]
struct CommentRecord {
    path: String,
    size_bytes: i64,
    modified_time: i64,
    comment: String,
    comment_norm: String,
}

// フェーズ1の走査スレッドと watcher が共通で必要とする書き込み先と共有状態。
// 個別に引数で回すと関数の引数が増えすぎるため、1つにまとめて持ち回る。
#[derive(Clone)]
struct ScanContext {
    db_path: PathBuf,
    write_tx: Sender<WriteCommand>,
    backfill_tx: Sender<BackfillMessage>,
    progress: Arc<Mutex<IndexProgress>>,
    shutdown: Arc<AtomicBool>,
}

impl ScanContext {
    // フェーズ2に取りこぼしがないか確認させる。writer へ積んだ書き込みが
    // コミット済みであることが確かな場所からだけ呼ぶ。
    fn kick_comment_backfill(&self) {
        let _ = self.backfill_tx.send(BackfillMessage::Kick);
    }

    // 直前に writer へ積んだ書き込みのコミット後にフェーズ2を起こす。
    // writer は受信順に処理するため、この通知が処理される時点で先行分は反映済み。
    fn kick_comment_backfill_after_writes(&self) {
        let _ = self.write_tx.send(WriteCommand::NotifyCommentPending);
    }

    fn update_progress(&self, edit: impl FnOnce(&mut IndexProgress)) {
        update_progress(&self.progress, edit);
    }

    fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug)]
struct WatchedRoot {
    root_id: i64,
    root_path: PathBuf,
}

#[derive(Debug)]
enum WatcherMessage {
    SetRoots(Vec<WatchedRoot>),
    Shutdown,
}

#[derive(Default)]
struct PendingChanges {
    path_changes: HashSet<PathBuf>,
    moves: Vec<(PathBuf, PathBuf)>,
    last_change_at: Option<Instant>,
}

impl SearchEngine {
    // エンジン起動時に DB を初期化し、writer/watcher スレッドを開始する。
    pub fn new(db_path: PathBuf) -> EngineResult<Self> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }

        let conn = open_connection(&db_path)?;
        apply_migrations(&conn)?;
        drop(conn);

        let (backfill_tx, backfill_rx) = mpsc::channel();
        let (write_tx, write_rx) = mpsc::channel();
        let db_for_writer = db_path.clone();
        let backfill_tx_for_writer = backfill_tx.clone();
        thread::spawn(move || writer_loop(db_for_writer, write_rx, backfill_tx_for_writer));

        let progress = Arc::new(Mutex::new(IndexProgress::default()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let scan_context = ScanContext {
            db_path: db_path.clone(),
            write_tx: write_tx.clone(),
            backfill_tx,
            progress,
            shutdown,
        };

        let backfill_context = scan_context.clone();
        thread::spawn(move || {
            comment_backfill_loop(
                backfill_rx,
                backfill_context.db_path,
                backfill_context.write_tx,
                backfill_context.progress,
                backfill_context.shutdown,
            )
        });

        let (watcher_tx, watcher_rx) = mpsc::channel();
        let watcher_context = scan_context.clone();
        thread::spawn(move || watcher_loop(watcher_rx, watcher_context));

        let engine = Self {
            inner: Arc::new(EngineInner {
                db_path,
                write_tx,
                watcher_tx,
                scan_context,
                index_event_txs: Mutex::new(Vec::new()),
            }),
        };

        engine.refresh_watcher_roots()?;
        // 前回の終了で取り残したコメント取得待ちを引き継ぐ。
        engine.inner.scan_context.kick_comment_backfill();
        Ok(engine)
    }

    // ffprobe の導入待ちで保留したコメント取得を、外部の準備完了に合わせて起こす。
    pub fn resume_comment_backfill(&self) {
        self.inner.scan_context.kick_comment_backfill();
    }

    // 進捗ウィンドウ用に現在の進捗を複製して返す。毎フレーム呼ばれるため軽い処理に保つ。
    pub fn progress(&self) -> IndexProgress {
        self.inner
            .scan_context
            .progress
            .lock()
            .map(|progress| *progress)
            .unwrap_or_default()
    }

    // UIがフルスキャンの開始・完了を受け取るための購読チャンネルを作る。
    pub fn subscribe_index_events(&self) -> mpsc::Receiver<IndexEvent> {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut subscribers) = self.inner.index_event_txs.lock() {
            subscribers.push(tx);
        }
        rx
    }

    // DB 上の監視ルート一覧を UI 用構造体で返す。
    pub fn list_roots(&self) -> EngineResult<Vec<RootEntry>> {
        let conn = open_connection(&self.inner.db_path)?;
        let mut stmt = conn
            .prepare(
                "SELECT root_id, root_path, is_enabled, last_scan_time
                 FROM roots
                 ORDER BY root_path COLLATE NOCASE ASC",
            )
            .map_err(|err| err.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RootEntry {
                    root_id: row.get(0)?,
                    root_path: row.get(1)?,
                    is_enabled: row.get::<_, i64>(2)? != 0,
                    last_scan_time: row.get(3)?,
                })
            })
            .map_err(|err| err.to_string())?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|err| err.to_string())?);
        }
        Ok(entries)
    }

    // 起動時用。ルート同期と全ルートのスキャンを1回の呼び出しでまとめる。
    // sync_roots と reindex_all_async を続けて呼ぶと、新規追加ルートを二重に走査してしまう。
    pub fn sync_roots_and_rescan(&self, desired_paths: &[PathBuf]) -> EngineResult<()> {
        self.sync_roots_with_target(desired_paths, IndexEventTarget::Main, ScanTrigger::AllRoots)
    }

    pub fn sync_roots_from_settings(&self, desired_paths: &[PathBuf]) -> EngineResult<()> {
        self.sync_roots_with_target(
            desired_paths,
            IndexEventTarget::Settings,
            ScanTrigger::NewRootsOnly,
        )
    }

    fn sync_roots_with_target(
        &self,
        desired_paths: &[PathBuf],
        target: IndexEventTarget,
        trigger: ScanTrigger,
    ) -> EngineResult<()> {
        let mut normalized_paths = Vec::new();
        let mut dedup = HashSet::new();

        for path in desired_paths {
            let normalized = normalize_root_path(path)?;
            if !normalized.is_dir() {
                return Err(format!(
                    "検索対象フォルダが存在しないか、ディレクトリではありません: {}",
                    normalized.to_string_lossy()
                ));
            }
            let key = path_to_key(&normalized);
            if dedup.insert(key.clone()) {
                normalized_paths.push((normalized, key));
            }
        }

        let current = self.list_roots()?;
        let current_map: HashMap<String, RootEntry> = current
            .iter()
            .cloned()
            .map(|entry| (entry.root_path.clone(), entry))
            .collect();

        let desired_set: HashSet<String> = normalized_paths
            .iter()
            .map(|(_, key)| key.clone())
            .collect();
        let mut scan_started = false;

        for (path, key) in &normalized_paths {
            let added_now = !current_map.contains_key(key);
            let root_id = self.add_or_enable_root(key)?;
            if added_now || matches!(trigger, ScanTrigger::AllRoots) {
                self.start_full_scan(root_id, path.clone(), target);
                scan_started = true;
            }
        }

        let mut removed_any = false;
        for entry in current {
            if !desired_set.contains(&entry.root_path) {
                self.remove_root(entry.root_id)?;
                removed_any = true;
            }
        }

        if removed_any && !scan_started {
            self.notify_index_event(IndexEvent::UpdateStarted { target });
            self.notify_index_event(IndexEvent::UpdateFinished {
                target,
                result: Ok(()),
            });
        }

        self.refresh_watcher_roots()?;
        Ok(())
    }

    // 有効ルートすべてに対して再インデックスを非同期起動する。
    #[cfg(test)]
    pub fn reindex_all_async(&self) -> EngineResult<()> {
        self.reindex_all_async_for(IndexEventTarget::Main)
            .map(|_| ())
    }

    pub fn reindex_all_from_settings_async(&self) -> EngineResult<usize> {
        self.reindex_all_async_for(IndexEventTarget::Settings)
    }

    fn reindex_all_async_for(&self, target: IndexEventTarget) -> EngineResult<usize> {
        let roots = self.list_roots()?;
        let enabled_roots = roots
            .into_iter()
            .filter(|root| root.is_enabled)
            .collect::<Vec<_>>();
        let root_count = enabled_roots.len();
        for root in enabled_roots {
            self.start_full_scan(root.root_id, PathBuf::from(root.root_path), target);
        }
        Ok(root_count)
    }

    // クエリを正規化し、空白区切りは AND、単語1つは prefix -> contains で検索する。
    pub fn search(&self, request: &SearchRequest) -> EngineResult<Vec<SearchHit>> {
        let conn = open_connection(&self.inner.db_path)?;
        let limit = request.limit.clamp(1, MAX_SEARCH_LIMIT);
        let normalized_query = normalize_query(&request.query);

        if normalized_query.is_empty() {
            return run_search_query(&conn, request, None, limit);
        }

        let mut terms = Vec::new();
        for term in normalized_query.split_whitespace() {
            if !terms.contains(&term) {
                terms.push(term);
            }
        }

        if terms.len() > 1 {
            let patterns = terms
                .into_iter()
                .map(|term| format!("%{}%", escape_like_pattern(term)))
                .collect();
            return run_search_query(
                &conn,
                request,
                Some(QueryPattern::AllTerms { patterns }),
                limit,
            );
        }

        let single_term = terms[0];
        let escaped = escape_like_pattern(single_term);
        let prefix_pattern = format!("{escaped}%");
        let contains_pattern = format!("%{escaped}%");

        let mut hits = run_search_query(
            &conn,
            request,
            Some(QueryPattern::Prefix {
                pattern: prefix_pattern.clone(),
                exact: single_term.to_string(),
            }),
            limit,
        )?;

        if hits.len() >= limit {
            return Ok(hits);
        }

        let remain = limit - hits.len();
        let mut contains_hits = run_search_query(
            &conn,
            request,
            Some(QueryPattern::Contains {
                pattern: contains_pattern,
                prefix_pattern,
            }),
            remain,
        )?;
        hits.append(&mut contains_hits);
        Ok(hits)
    }

    #[cfg(test)]
    pub fn apply_path_change(
        &self,
        old_path: Option<&std::path::Path>,
        new_path: Option<&std::path::Path>,
    ) -> EngineResult<()> {
        let roots = self.enabled_watched_roots()?;
        if let Some(old) = old_path {
            watcher::apply_delete_change(old, &self.inner.scan_context)?;
        }
        if let Some(new_path) = new_path {
            watcher::apply_upsert_change(new_path, &roots, &self.inner.scan_context)?;
        }
        Ok(())
    }

    fn add_or_enable_root(&self, root_path: &str) -> EngineResult<i64> {
        let (tx, rx) = mpsc::channel();
        self.inner
            .write_tx
            .send(WriteCommand::AddOrEnableRoot {
                root_path: root_path.to_string(),
                resp: tx,
            })
            .map_err(|err| err.to_string())?;
        rx.recv().map_err(|err| err.to_string())?
    }

    fn remove_root(&self, root_id: i64) -> EngineResult<()> {
        let (tx, rx) = mpsc::channel();
        self.inner
            .write_tx
            .send(WriteCommand::RemoveRoot { root_id, resp: tx })
            .map_err(|err| err.to_string())?;
        rx.recv().map_err(|err| err.to_string())?
    }

    // watcher スレッドへ最新 root セットを通知する。
    fn refresh_watcher_roots(&self) -> EngineResult<()> {
        let roots = self.enabled_watched_roots()?;
        self.inner
            .watcher_tx
            .send(WatcherMessage::SetRoots(roots))
            .map_err(|err| err.to_string())
    }

    fn enabled_watched_roots(&self) -> EngineResult<Vec<WatchedRoot>> {
        let roots = self.list_roots()?;
        Ok(roots
            .into_iter()
            .filter(|root| root.is_enabled)
            .map(|root| WatchedRoot {
                root_id: root.root_id,
                root_path: PathBuf::from(root.root_path),
            })
            .collect())
    }

    // ルート単位のフェーズ1（ファイル名インデックス）をバックグラウンドで起動する。
    fn start_full_scan(&self, root_id: i64, root_path: PathBuf, target: IndexEventTarget) {
        self.notify_index_event(IndexEvent::UpdateStarted { target });
        let engine = self.clone();
        let context = self.inner.scan_context.clone();
        context.update_progress(|progress| {
            progress.scanning_roots = progress.scanning_roots.saturating_add(1);
        });
        thread::spawn(move || {
            let result = scan_root(root_id, &root_path, &context);
            if let Err(err) = &result {
                crate::log_error!(
                    Search,
                    "フォルダの全走査に失敗しました: {} ({err})",
                    root_path.to_string_lossy()
                );
            }
            context.update_progress(|progress| {
                progress.scanning_roots = progress.scanning_roots.saturating_sub(1);
                if progress.scanning_roots == 0 {
                    progress.scanned_files = 0;
                }
            });
            // ファイル名の反映が済んだので、増えたコメント取得待ちを拾わせる。
            context.kick_comment_backfill();
            engine.notify_index_event(IndexEvent::UpdateFinished { target, result });
        });
    }

    fn notify_index_event(&self, event: IndexEvent) {
        if let Ok(mut subscribers) = self.inner.index_event_txs.lock() {
            subscribers.retain(|tx| tx.send(event.clone()).is_ok());
        }
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        self.scan_context.shutdown.store(true, Ordering::Relaxed);
        let _ = self
            .scan_context
            .backfill_tx
            .send(BackfillMessage::Shutdown);
        let _ = self.watcher_tx.send(WatcherMessage::Shutdown);
        let _ = self.write_tx.send(WriteCommand::Shutdown);
    }
}

// 進捗を排他で書き換える。ロックが壊れていても走査自体は続けたいので失敗は握り潰す。
fn update_progress(progress: &Mutex<IndexProgress>, edit: impl FnOnce(&mut IndexProgress)) {
    if let Ok(mut progress) = progress.lock() {
        edit(&mut progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_dummy(path: &std::path::Path, bytes: usize) {
        let data = vec![0_u8; bytes];
        fs::write(path, data).expect("write dummy file");
    }

    // フェーズ2（コメント補完）まで含めてインデックス作成が落ち着くのを待つ。
    //
    // ffprobe が未導入の環境ではフェーズ2は取得待ちを残したまま抜ける（後で取り直せる
    // ようにするため）。その場合は取得待ちが捌けることを待機条件にできない。
    fn wait_for_index_idle(engine: &SearchEngine) {
        let expects_drained = crate::paths::ffprobe_ready();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let drained = !expects_drained || count_pending_comments(engine) == 0;
            if !engine.progress().is_active() && drained {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("インデックス作成が終わらなかった");
    }

    fn count_pending_comments(engine: &SearchEngine) -> i64 {
        let conn = open_connection(&engine.inner.db_path).expect("open index");
        conn.query_row(
            "SELECT COUNT(*) FROM files WHERE comment_norm IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("count pending")
    }

    fn setup_engine() -> (tempfile::TempDir, SearchEngine) {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("index.db");
        let engine = SearchEngine::new(db_path).expect("engine init");
        (dir, engine)
    }

    #[test]
    fn normalizes_and_escapes_query() {
        assert_eq!(normalize_query(" ＡＢＣ_旅行% "), "abc_旅行%");
        assert_eq!(escape_like_pattern("abc_旅行%"), "abc\\_旅行\\%");
    }

    #[test]
    fn indexes_and_searches_supported_video_formats() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");

        write_dummy(&root.join("旅行_沖縄.mp4"), 64);
        write_dummy(&root.join("会議録画_2026.mov"), 64);
        write_dummy(&root.join("素材.m4v"), 64);
        write_dummy(&root.join("透過.webm"), 64);
        write_dummy(&root.join("保存.MKV"), 64);
        write_dummy(&root.join("ignore.txt"), 64);

        engine
            .sync_roots_and_rescan(std::slice::from_ref(&root))
            .expect("sync roots");
        engine.reindex_all_async().expect("reindex all");
        thread::sleep(Duration::from_millis(350));

        let hits = engine
            .search(&SearchRequest {
                query: "旅行".to_string(),
                limit: 20,
                ..Default::default()
            })
            .expect("search by japanese");

        assert_eq!(hits.len(), 1);
        assert!(hits[0].file_name.contains("旅行_沖縄"));

        let all_hits = engine
            .search(&SearchRequest {
                limit: 20,
                ..Default::default()
            })
            .expect("search all supported videos");
        assert_eq!(all_hits.len(), 5);
    }

    #[test]
    fn reports_completion_after_index_writes_are_committed() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");
        write_dummy(&root.join("完了通知.mp4"), 64);
        let events = engine.subscribe_index_events();

        engine.sync_roots_and_rescan(&[root]).expect("sync roots");
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(2)),
            Ok(IndexEvent::UpdateStarted {
                target: IndexEventTarget::Main
            })
        ));
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(2)),
            Ok(IndexEvent::UpdateFinished {
                target: IndexEventTarget::Main,
                result: Ok(())
            })
        ));

        let hits = engine
            .search(&SearchRequest {
                query: "完了通知".to_string(),
                limit: 20,
                ..Default::default()
            })
            .expect("search immediately after completion event");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn indexes_file_names_before_reading_comments() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");
        write_dummy(&root.join("先に出る名前.mp4"), 64);
        let events = engine.subscribe_index_events();

        engine.sync_roots_and_rescan(&[root]).expect("sync roots");
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)),
            Ok(IndexEvent::UpdateStarted { .. })
        ));
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)),
            Ok(IndexEvent::UpdateFinished { result: Ok(()), .. })
        ));

        // フェーズ1が終わった時点でファイル名検索はできる。
        let hits = engine
            .search(&SearchRequest {
                query: "先に出る名前".to_string(),
                limit: 20,
                ..Default::default()
            })
            .expect("search right after phase 1");
        assert_eq!(hits.len(), 1);

        // ffprobe が中身を読めないダミーファイルでもフェーズ2は取得待ちを残さず終わる。
        // 残すと同じ行を引き当て続けて永久に回り続けてしまう。
        wait_for_index_idle(&engine);
    }

    #[test]
    fn supports_metadata_filters() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");

        write_dummy(&root.join("small.mp4"), 8);
        write_dummy(&root.join("large.mp4"), 8_192);

        engine
            .sync_roots_and_rescan(std::slice::from_ref(&root))
            .expect("sync roots");
        engine.reindex_all_async().expect("reindex all");
        thread::sleep(Duration::from_millis(350));

        let hits = engine
            .search(&SearchRequest {
                query: String::new(),
                size_min: Some(1_024),
                limit: 20,
                ..Default::default()
            })
            .expect("search by size");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_name, "large.mp4");
    }

    #[test]
    fn applies_add_delete_rename_updates() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");

        engine
            .sync_roots_and_rescan(std::slice::from_ref(&root))
            .expect("sync roots");
        thread::sleep(Duration::from_millis(200));

        let added = root.join("追加.mp4");
        write_dummy(&added, 32);
        engine
            .apply_path_change(None, Some(&added))
            .expect("apply add");
        thread::sleep(Duration::from_millis(120));

        let mut hits = engine
            .search(&SearchRequest {
                query: "追加".to_string(),
                limit: 20,
                ..Default::default()
            })
            .expect("search after add");
        assert_eq!(hits.len(), 1);

        let renamed = root.join("変更後.mp4");
        fs::rename(&added, &renamed).expect("rename");
        engine
            .apply_path_change(Some(&added), Some(&renamed))
            .expect("apply rename");
        thread::sleep(Duration::from_millis(120));

        hits = engine
            .search(&SearchRequest {
                query: "変更後".to_string(),
                limit: 20,
                ..Default::default()
            })
            .expect("search renamed");
        assert_eq!(hits.len(), 1);

        fs::remove_file(&renamed).expect("remove file");
        engine
            .apply_path_change(Some(&renamed), None)
            .expect("apply delete");
        thread::sleep(Duration::from_millis(120));

        hits = engine
            .search(&SearchRequest {
                query: "変更後".to_string(),
                limit: 20,
                ..Default::default()
            })
            .expect("search after delete");
        assert!(hits.is_empty());
    }

    #[test]
    fn searches_literal_percent_and_underscore() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");

        write_dummy(&root.join("100%_test.mp4"), 64);
        engine
            .sync_roots_and_rescan(std::slice::from_ref(&root))
            .expect("sync roots");
        engine.reindex_all_async().expect("reindex all");
        thread::sleep(Duration::from_millis(350));

        let hits = engine
            .search(&SearchRequest {
                query: "100%_".to_string(),
                limit: 20,
                ..Default::default()
            })
            .expect("search escaped");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_name, "100%_test.mp4");
    }

    #[test]
    fn searches_whitespace_separated_terms_with_and_semantics() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");

        write_dummy(&root.join("ふ・れ・ん・ど・し・た・い.mp4"), 64);
        write_dummy(&root.join("ふ・た・り.mp4"), 64);
        engine
            .sync_roots_and_rescan(std::slice::from_ref(&root))
            .expect("sync roots");
        engine.reindex_all_async().expect("reindex all");
        thread::sleep(Duration::from_millis(350));

        let hits = engine
            .search(&SearchRequest {
                query: "ふ れ".to_string(),
                limit: 20,
                sort: SearchSort::NameAsc,
                ..Default::default()
            })
            .expect("search by multiple terms");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_name, "ふ・れ・ん・ど・し・た・い.mp4");

        let reversed_hits = engine
            .search(&SearchRequest {
                query: "れ ふ".to_string(),
                limit: 20,
                sort: SearchSort::NameAsc,
                ..Default::default()
            })
            .expect("search by reversed terms");
        assert_eq!(reversed_hits.len(), 1);
    }

    #[test]
    fn searches_video_comments_and_combines_them_with_file_names() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");
        let video_path = root.join("ライブ映像.mp4");
        write_dummy(&video_path, 64);

        engine
            .sync_roots_and_rescan(std::slice::from_ref(&root))
            .expect("sync roots");
        // 手で入れたコメントをフェーズ2に上書きされないよう、収まるまで待つ。
        wait_for_index_idle(&engine);

        let conn = open_connection(&engine.inner.db_path).expect("open index");
        conn.execute(
            "UPDATE files SET comment = ?, comment_norm = ? WHERE path = ?",
            (
                "これはYouTubeの概要欄です",
                normalize::normalize_for_search("これはYouTubeの概要欄です"),
                path_to_key(&video_path),
            ),
        )
        .expect("store comment metadata");

        let comment_hits = engine
            .search(&SearchRequest {
                query: "youtube".to_string(),
                limit: 20,
                sort: SearchSort::NameAsc,
                ..Default::default()
            })
            .expect("search comment");
        assert_eq!(comment_hits.len(), 1);

        let combined_hits = engine
            .search(&SearchRequest {
                query: "ライブ 概要欄".to_string(),
                limit: 20,
                sort: SearchSort::NameAsc,
                ..Default::default()
            })
            .expect("search across file name and comment");
        assert_eq!(combined_hits.len(), 1);
    }

    #[test]
    fn searches_katakana_with_hiragana_query() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");
        write_dummy(&root.join("ウザい映像.mp4"), 64);

        engine
            .sync_roots_and_rescan(std::slice::from_ref(&root))
            .expect("sync roots");
        engine.reindex_all_async().expect("reindex all");
        thread::sleep(Duration::from_millis(350));

        let hits = engine
            .search(&SearchRequest {
                query: "うざ".to_string(),
                limit: 20,
                sort: SearchSort::NameAsc,
                ..Default::default()
            })
            .expect("search katakana with hiragana");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_name, "ウザい映像.mp4");
    }
}
