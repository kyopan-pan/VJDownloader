use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use walkdir::WalkDir;

use super::db::open_connection;
use super::normalize::{
    epoch_millis, epoch_secs, is_supported_video_path, normalize_for_search, path_to_key,
    system_time_to_epoch_secs,
};
use super::{EngineResult, FileRecord, ScanContext, UPSERT_BATCH_SIZE, WatchedRoot, WriteCommand};

type ScanKey = (PathBuf, i64);
type ScanLockMap = HashMap<ScanKey, Arc<Mutex<()>>>;

static FULL_SCAN_LOCKS: LazyLock<Mutex<ScanLockMap>> = LazyLock::new(|| Mutex::new(HashMap::new()));

// watcher 異常時のフォールバックとして、DB上の有効ルートを全量再走査する。
pub(super) fn trigger_reindex_all_from_db(context: &ScanContext) {
    let conn = match open_connection(&context.db_path) {
        Ok(conn) => conn,
        Err(err) => {
            crate::log_error!(Search, "再インデックス用のDBを開けませんでした: {err}");
            return;
        }
    };

    let mut stmt = match conn.prepare("SELECT root_id, root_path FROM roots WHERE is_enabled = 1") {
        Ok(stmt) => stmt,
        Err(err) => {
            crate::log_error!(
                Search,
                "再インデックス対象フォルダの照会に失敗しました: {err}"
            );
            return;
        }
    };

    let rows = match stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(rows) => rows,
        Err(err) => {
            crate::log_error!(
                Search,
                "再インデックス対象フォルダの読み出しに失敗しました: {err}"
            );
            return;
        }
    };

    for row in rows {
        let Ok((root_id, root_path)) = row else {
            continue;
        };
        let root_path = PathBuf::from(root_path);
        let context = context.clone();
        thread::spawn(move || {
            if let Err(err) = scan_root(root_id, &root_path, &context) {
                crate::log_error!(
                    Search,
                    "再インデックスに失敗しました: {} ({err})",
                    root_path.to_string_lossy()
                );
            }
            context.kick_comment_backfill();
        });
    }
}

// 監視対象ルートのうち、対象パスに最も深く一致する root_id を返す。
pub(super) fn find_root_id_for_path(path: &Path, roots: &[WatchedRoot]) -> Option<i64> {
    let mut best_match: Option<(usize, i64)> = None;

    for root in roots {
        if path.starts_with(&root.root_path) {
            let len = root.root_path.as_os_str().len();
            match best_match {
                Some((best_len, _)) if best_len >= len => {}
                _ => best_match = Some((len, root.root_id)),
            }
        }
    }

    best_match.map(|(_, root_id)| root_id)
}

// 指定ルートを全走査し、ファイル名インデックス（フェーズ1）を作り直す。
// 動画コメントはここでは読まない。1ファイルごとに ffprobe を起動すると10万件規模で
// 数時間かかり、その間まったく検索できなくなるため、コメントは comments.rs の
// バックフィルへ委ねて後から埋める。
pub(super) fn scan_root(root_id: i64, root_path: &Path, context: &ScanContext) -> EngineResult<()> {
    let scan_lock = {
        let mut locks = FULL_SCAN_LOCKS.lock().map_err(|err| err.to_string())?;
        locks
            .entry((context.db_path.clone(), root_id))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _scan_guard = scan_lock.lock().map_err(|err| err.to_string())?;

    if !root_path.exists() {
        return Ok(());
    }

    let marker = epoch_millis();
    let mut batch = Vec::with_capacity(UPSERT_BATCH_SIZE);

    for entry in WalkDir::new(root_path).into_iter().filter_map(Result::ok) {
        if context.is_shutting_down() {
            return Ok(());
        }

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if !is_supported_video_path(path) {
            continue;
        }

        // Windows では列挙時に取得済みのメタデータが DirEntry にキャッシュされている。
        // fs::metadata を呼び直すとファイル数と同じ回数だけ余計にファイルを開くことになる。
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if let Some(record) = build_record(root_id, path, &metadata, marker) {
            batch.push(record);
            context.update_progress(|progress| {
                progress.scanned_files = progress.scanned_files.saturating_add(1);
            });
        }

        flush_upsert_batch_if_full(&mut batch, &context.write_tx)?;
    }

    flush_upsert_batch(&mut batch, &context.write_tx)?;

    let (resp_tx, resp_rx) = std::sync::mpsc::channel();
    context
        .write_tx
        .send(WriteCommand::FinalizeScan {
            root_id,
            marker,
            finished_at: epoch_secs(),
            resp: resp_tx,
        })
        .map_err(|err| err.to_string())?;
    resp_rx.recv().map_err(|err| err.to_string())?
}

// ディレクトリ配下の対応動画を差分反映用に走査して upsert する。
pub(super) fn upsert_directory(
    dir: &Path,
    roots: &[WatchedRoot],
    context: &ScanContext,
) -> EngineResult<()> {
    let marker = epoch_millis();
    let mut batch = Vec::with_capacity(UPSERT_BATCH_SIZE);

    for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_supported_video_path(path) {
            continue;
        }

        let Some(root_id) = find_root_id_for_path(path, roots) else {
            continue;
        };

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if let Some(record) = build_record(root_id, path, &metadata, marker) {
            batch.push(record);
        }

        flush_upsert_batch_if_full(&mut batch, &context.write_tx)?;
    }

    flush_upsert_batch(&mut batch, &context.write_tx)?;

    Ok(())
}

fn flush_upsert_batch_if_full(
    batch: &mut Vec<FileRecord>,
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    if batch.len() < UPSERT_BATCH_SIZE {
        return Ok(());
    }
    flush_upsert_batch(batch, write_tx)
}

fn flush_upsert_batch(
    batch: &mut Vec<FileRecord>,
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    if batch.is_empty() {
        return Ok(());
    }

    write_tx
        .send(WriteCommand::UpsertFiles {
            files: std::mem::take(batch),
        })
        .map_err(|err| err.to_string())
}

// ファイルメタデータからフェーズ1の upsert 用レコードを組み立てる。
// メタデータは呼び出し側が取得済みのものを渡す。走査中に取り直すとファイル数ぶんの
// 追加 I/O になるため。
pub(super) fn build_record(
    root_id: i64,
    path: &Path,
    metadata: &fs::Metadata,
    marker: i64,
) -> Option<FileRecord> {
    if !metadata.is_file() {
        return None;
    }

    let file_name = path.file_name()?.to_string_lossy().to_string();
    let parent_dir = path.parent().map(path_to_key).unwrap_or_default();
    let modified_time = metadata
        .modified()
        .map(system_time_to_epoch_secs)
        .unwrap_or_else(|_| 0);

    Some(FileRecord {
        path: path_to_key(path),
        root_id,
        file_name_norm: normalize_for_search(&file_name),
        file_name,
        parent_dir,
        size_bytes: metadata.len() as i64,
        modified_time,
        created_time: metadata.created().map(system_time_to_epoch_secs).ok(),
        last_indexed_time: marker,
    })
}
