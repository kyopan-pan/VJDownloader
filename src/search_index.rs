mod db;
mod normalize;
mod query;
mod scanner;
mod watcher;
mod writer;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use db::{apply_migrations, open_connection};
use normalize::{escape_like_pattern, normalize_query, normalize_root_path, path_to_key};
use query::{run_search_query, QueryPattern};
use scanner::scan_root;
use watcher::watcher_loop;
use writer::writer_loop;

const DB_SCHEMA_VERSION: i32 = 1;
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(700);
const UPSERT_BATCH_SIZE: usize = 256;
const MAX_SEARCH_LIMIT: usize = 1_000;

pub type EngineResult<T> = Result<T, String>;

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
    },
    Shutdown,
}

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

        let (write_tx, write_rx) = mpsc::channel();
        let db_for_writer = db_path.clone();
        thread::spawn(move || writer_loop(db_for_writer, write_rx));

        let (watcher_tx, watcher_rx) = mpsc::channel();
        let watcher_write_tx = write_tx.clone();
        let watcher_db = db_path.clone();
        thread::spawn(move || watcher_loop(watcher_rx, watcher_write_tx, watcher_db));

        let engine = Self {
            inner: Arc::new(EngineInner {
                db_path,
                write_tx,
                watcher_tx,
            }),
        };

        engine.refresh_watcher_roots()?;
        Ok(engine)
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

    // desired ルート集合と DB の差分を同期し、必要な full scan を起動する。
    pub fn sync_roots(&self, desired_paths: &[PathBuf]) -> EngineResult<()> {
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

        for (path, key) in &normalized_paths {
            let added_now = !current_map.contains_key(key);
            let root_id = self.add_or_enable_root(key)?;
            if added_now {
                self.start_full_scan(root_id, path.clone());
            }
        }

        for entry in current {
            if !desired_set.contains(&entry.root_path) {
                self.remove_root(entry.root_id)?;
            }
        }

        self.refresh_watcher_roots()?;
        Ok(())
    }

    // 有効ルートすべてに対して再インデックスを非同期起動する。
    pub fn reindex_all_async(&self) -> EngineResult<()> {
        let roots = self.list_roots()?;
        for root in roots.into_iter().filter(|root| root.is_enabled) {
            self.start_full_scan(root.root_id, PathBuf::from(root.root_path));
        }
        Ok(())
    }

    // クエリを正規化し、prefix -> contains の順で段階検索する。
    pub fn search(&self, request: &SearchRequest) -> EngineResult<Vec<SearchHit>> {
        let conn = open_connection(&self.inner.db_path)?;
        let limit = request.limit.clamp(1, MAX_SEARCH_LIMIT);
        let normalized_query = normalize_query(&request.query);

        if normalized_query.is_empty() {
            return run_search_query(&conn, request, None, limit);
        }

        let escaped = escape_like_pattern(&normalized_query);
        let prefix_pattern = format!("{escaped}%");
        let contains_pattern = format!("%{escaped}%");

        let mut hits = run_search_query(
            &conn,
            request,
            Some(QueryPattern::Prefix {
                pattern: prefix_pattern.clone(),
                exact: normalized_query.clone(),
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
            watcher::apply_delete_change(old, &roots, &self.inner.write_tx)?;
        }
        if let Some(new_path) = new_path {
            watcher::apply_upsert_change(new_path, &roots, &self.inner.write_tx)?;
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

    // ルート単位の full scan をバックグラウンドで起動する。
    fn start_full_scan(&self, root_id: i64, root_path: PathBuf) {
        let write_tx = self.inner.write_tx.clone();
        thread::spawn(move || {
            if let Err(err) = scan_root(root_id, &root_path, &write_tx) {
                eprintln!(
                    "[search-index] full scan failed for {}: {}",
                    root_path.to_string_lossy(),
                    err
                );
            }
        });
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        let _ = self.watcher_tx.send(WatcherMessage::Shutdown);
        let _ = self.write_tx.send(WriteCommand::Shutdown);
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
    fn indexes_and_searches_japanese_mp4() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");

        write_dummy(&root.join("旅行_沖縄.mp4"), 64);
        write_dummy(&root.join("会議録画_2026.mp4"), 64);
        write_dummy(&root.join("ignore.txt"), 64);

        engine.sync_roots(&[root.clone()]).expect("sync roots");
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
    }

    #[test]
    fn supports_metadata_filters() {
        let (temp, engine) = setup_engine();
        let root = temp.path().join("videos");
        fs::create_dir_all(&root).expect("create root");

        write_dummy(&root.join("small.mp4"), 8);
        write_dummy(&root.join("large.mp4"), 8_192);

        engine.sync_roots(&[root.clone()]).expect("sync roots");
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

        engine.sync_roots(&[root.clone()]).expect("sync roots");
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
        engine.sync_roots(&[root.clone()]).expect("sync roots");
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
}
