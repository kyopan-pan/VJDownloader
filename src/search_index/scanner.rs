use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use walkdir::WalkDir;

use super::db::open_connection;
use super::normalize::{
    epoch_millis, epoch_secs, is_mp4_path, normalize_for_search, path_to_key,
    system_time_to_epoch_secs,
};
use super::{EngineResult, FileRecord, UPSERT_BATCH_SIZE, WatchedRoot, WriteCommand};

// watcher 異常時のフォールバックとして、DB上の有効ルートを全量再走査する。
pub(super) fn trigger_reindex_all_from_db(db_path: &Path, write_tx: &Sender<WriteCommand>) {
    let conn = match open_connection(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            eprintln!("[search-index] failed to open DB for fallback reindex: {err}");
            return;
        }
    };

    let mut stmt = match conn.prepare("SELECT root_id, root_path FROM roots WHERE is_enabled = 1") {
        Ok(stmt) => stmt,
        Err(err) => {
            eprintln!("[search-index] failed to query roots for fallback reindex: {err}");
            return;
        }
    };

    let rows = match stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("[search-index] failed to iterate roots for fallback reindex: {err}");
            return;
        }
    };

    for row in rows {
        let Ok((root_id, root_path)) = row else {
            continue;
        };
        let root_path = PathBuf::from(root_path);
        let write_tx = write_tx.clone();
        thread::spawn(move || {
            if let Err(err) = scan_root(root_id, &root_path, &write_tx) {
                eprintln!(
                    "[search-index] fallback reindex failed for {}: {}",
                    root_path.to_string_lossy(),
                    err
                );
            }
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

// 指定ルートを全走査して MP4 を再インデックスする。
pub(super) fn scan_root(
    root_id: i64,
    root_path: &Path,
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    if !root_path.exists() {
        return Ok(());
    }

    let marker = epoch_millis();
    let mut batch = Vec::with_capacity(UPSERT_BATCH_SIZE);

    for entry in WalkDir::new(root_path).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if !is_mp4_path(path) {
            continue;
        }

        if let Some(record) = build_record_from_path(root_id, path, marker) {
            batch.push(record);
        }

        flush_upsert_batch_if_full(&mut batch, write_tx)?;
    }

    flush_upsert_batch(&mut batch, write_tx)?;

    write_tx
        .send(WriteCommand::FinalizeScan {
            root_id,
            marker,
            finished_at: epoch_secs(),
        })
        .map_err(|err| err.to_string())?;

    Ok(())
}

// ディレクトリ配下の MP4 を差分反映用に走査して upsert する。
pub(super) fn upsert_directory(
    dir: &Path,
    roots: &[WatchedRoot],
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    let marker = epoch_millis();
    let mut batch = Vec::with_capacity(UPSERT_BATCH_SIZE);

    for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_mp4_path(path) {
            continue;
        }

        let Some(root_id) = find_root_id_for_path(path, roots) else {
            continue;
        };

        if let Some(record) = build_record_from_path(root_id, path, marker) {
            batch.push(record);
        }

        flush_upsert_batch_if_full(&mut batch, write_tx)?;
    }

    flush_upsert_batch(&mut batch, write_tx)?;

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

// ファイルメタデータから DB upsert 用レコードを組み立てる。
pub(super) fn build_record_from_path(root_id: i64, path: &Path, marker: i64) -> Option<FileRecord> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }

    let file_name = path.file_name()?.to_string_lossy().to_string();
    let parent_dir = path.parent().map(path_to_key).unwrap_or_else(String::new);
    let modified_time = metadata
        .modified()
        .map(system_time_to_epoch_secs)
        .unwrap_or_else(|_| 0);
    let created_time = metadata.created().map(system_time_to_epoch_secs).ok();

    Some(FileRecord {
        path: path_to_key(path),
        root_id,
        file_name_norm: normalize_for_search(&file_name),
        file_name,
        parent_dir,
        size_bytes: metadata.len() as i64,
        modified_time,
        created_time,
        last_indexed_time: marker,
    })
}
