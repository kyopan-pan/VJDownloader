use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use walkdir::WalkDir;

use crate::paths::ffprobe_path;

use super::db::open_connection;
use super::normalize::{
    epoch_millis, epoch_secs, is_supported_video_path, normalize_for_search, path_to_key,
    system_time_to_epoch_secs,
};
use super::{EngineResult, FileRecord, UPSERT_BATCH_SIZE, WatchedRoot, WriteCommand};

type ScanKey = (PathBuf, i64);
type ScanLockMap = HashMap<ScanKey, Arc<Mutex<()>>>;

static FULL_SCAN_LOCKS: LazyLock<Mutex<ScanLockMap>> = LazyLock::new(|| Mutex::new(HashMap::new()));

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
        let db_path = db_path.to_path_buf();
        thread::spawn(move || {
            if let Err(err) = scan_root(root_id, &root_path, &db_path, &write_tx) {
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

// 指定ルートを全走査して対応動画を再インデックスする。
pub(super) fn scan_root(
    root_id: i64,
    root_path: &Path,
    db_path: &Path,
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    let scan_lock = {
        let mut locks = FULL_SCAN_LOCKS.lock().map_err(|err| err.to_string())?;
        locks
            .entry((db_path.to_path_buf(), root_id))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _scan_guard = scan_lock.lock().map_err(|err| err.to_string())?;

    if !root_path.exists() {
        return Ok(());
    }

    let marker = epoch_millis();
    let mut batch = Vec::with_capacity(UPSERT_BATCH_SIZE);
    let cached_files = load_cached_files(db_path, root_id)?;

    for entry in WalkDir::new(root_path).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if !is_supported_video_path(path) {
            continue;
        }

        let path_key = path_to_key(path);
        if let Some(record) =
            build_record_from_path(root_id, path, marker, cached_files.get(&path_key))
        {
            batch.push(record);
        }

        flush_upsert_batch_if_full(&mut batch, write_tx)?;
    }

    flush_upsert_batch(&mut batch, write_tx)?;

    let (resp_tx, resp_rx) = std::sync::mpsc::channel();
    write_tx
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
    write_tx: &Sender<WriteCommand>,
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

        if let Some(record) = build_record_from_path(root_id, path, marker, None) {
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
pub(super) fn build_record_from_path(
    root_id: i64,
    path: &Path,
    marker: i64,
    cached: Option<&CachedFile>,
) -> Option<FileRecord> {
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
    let size_bytes = metadata.len() as i64;
    let (comment, comment_norm) = match cached.filter(|cached| {
        cached.size_bytes == size_bytes
            && cached.modified_time == modified_time
            && cached.comment_norm.is_some()
    }) {
        Some(cached) => (
            cached.comment.clone(),
            Some(normalize_for_search(&cached.comment)),
        ),
        None => match read_search_comment(path) {
            Ok(comment) => {
                let normalized = normalize_for_search(&comment);
                (comment, Some(normalized))
            }
            Err(err) => {
                eprintln!(
                    "[search-index] failed to read video comment from {}: {}",
                    path.to_string_lossy(),
                    err
                );
                (String::new(), None)
            }
        },
    };

    Some(FileRecord {
        path: path_to_key(path),
        root_id,
        file_name_norm: normalize_for_search(&file_name),
        file_name,
        comment,
        comment_norm,
        parent_dir,
        size_bytes,
        modified_time,
        created_time,
        last_indexed_time: marker,
    })
}

#[derive(Clone)]
pub(super) struct CachedFile {
    size_bytes: i64,
    modified_time: i64,
    comment: String,
    comment_norm: Option<String>,
}

// 未変更ファイルで ffprobe を再実行しないよう、前回取得したコメントを読み込む。
fn load_cached_files(db_path: &Path, root_id: i64) -> EngineResult<HashMap<String, CachedFile>> {
    let conn = open_connection(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT path, size_bytes, modified_time, comment, comment_norm
             FROM files
             WHERE root_id = ?",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([root_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                CachedFile {
                    size_bytes: row.get(1)?,
                    modified_time: row.get(2)?,
                    comment: row.get(3)?,
                    comment_norm: row.get(4)?,
                },
            ))
        })
        .map_err(|err| err.to_string())?;

    let mut files = HashMap::new();
    for row in rows {
        let (path, cached) = row.map_err(|err| err.to_string())?;
        files.insert(path, cached);
    }
    Ok(files)
}

// ffprobeからコメント系タグを取得する。タグ名の大文字・小文字は区別しない。
fn read_search_comment(path: &Path) -> EngineResult<String> {
    let output = Command::new(ffprobe_path())
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format_tags")
        .arg("-of")
        .arg("json")
        .arg(path)
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }

    let value: Value = serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())?;
    Ok(parse_search_comment(&value))
}

fn parse_search_comment(value: &Value) -> String {
    let Some(tags) = value
        .get("format")
        .and_then(|format| format.get("tags"))
        .and_then(Value::as_object)
    else {
        return String::new();
    };

    let mut values = Vec::<String>::new();
    for (key, value) in tags {
        if !key.eq_ignore_ascii_case("comment") && !key.eq_ignore_ascii_case("description") {
            continue;
        }
        let Some(text) = value
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        if !values.iter().any(|existing| existing == text) {
            values.push(text.to_string());
        }
    }
    values.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::parse_search_comment;

    #[test]
    fn parses_comment_and_description_tags_case_insensitively() {
        let value = json!({
            "format": {
                "tags": {
                    "COMMENT": "YouTubeの概要欄",
                    "description": "補足説明",
                    "title": "検索対象外"
                }
            }
        });

        assert_eq!(parse_search_comment(&value), "YouTubeの概要欄\n補足説明");
    }
}
