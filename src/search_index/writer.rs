use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use super::db::{apply_migrations, open_connection};
use super::{EngineResult, WriteCommand};

// 書き込み専用スレッドでコマンドを順次適用する。
pub(super) fn writer_loop(db_path: PathBuf, rx: Receiver<WriteCommand>) {
    let mut conn = match open_connection(&db_path).and_then(|conn| {
        apply_migrations(&conn)?;
        Ok(conn)
    }) {
        Ok(conn) => conn,
        Err(err) => {
            crate::log_error!(Search, "検索インデックスのDB初期化に失敗しました: {err}");
            return;
        }
    };

    while let Ok(cmd) = rx.recv() {
        if let WriteCommand::Shutdown = cmd {
            break;
        }

        if let Err(err) = apply_write_command(&mut conn, cmd) {
            crate::log_error!(Search, "検索インデックスの更新に失敗しました: {err}");
        }
    }
}

// あるパス配下（`prefix` + 区切り文字で始まる）だけを含む、BINARY 照合での半開区間を返す。
// 区切り文字は ASCII なので、上限は区切り文字を1つ進めた文字で作れる。
fn descendant_path_range(prefix: &str) -> (String, String) {
    let sep = if prefix.contains('\\') { b'\\' } else { b'/' };
    let lower = format!("{prefix}{}", sep as char);
    let upper = format!("{prefix}{}", (sep + 1) as char);
    (lower, upper)
}

// 受信した DB 更新コマンドをトランザクション付きで実行する。
pub(super) fn apply_write_command(conn: &mut Connection, cmd: WriteCommand) -> EngineResult<()> {
    match cmd {
        WriteCommand::AddOrEnableRoot { root_path, resp } => {
            let result = (|| {
                let existing: Option<i64> = conn
                    .query_row(
                        "SELECT root_id FROM roots WHERE root_path = ?",
                        [root_path.as_str()],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|err| err.to_string())?;

                if let Some(root_id) = existing {
                    conn.execute(
                        "UPDATE roots SET is_enabled = 1 WHERE root_id = ?",
                        [root_id],
                    )
                    .map_err(|err| err.to_string())?;
                    return Ok(root_id);
                }

                conn.execute(
                    "INSERT INTO roots (root_path, is_enabled) VALUES (?, 1)",
                    [root_path.as_str()],
                )
                .map_err(|err| err.to_string())?;

                Ok(conn.last_insert_rowid())
            })();

            let _ = resp.send(result);
        }
        WriteCommand::RemoveRoot { root_id, resp } => {
            let result = conn
                .execute("DELETE FROM roots WHERE root_id = ?", [root_id])
                .map(|_| ())
                .map_err(|err| err.to_string());
            let _ = resp.send(result);
        }
        WriteCommand::UpsertFiles { files } => {
            if files.is_empty() {
                return Ok(());
            }

            let tx = conn.transaction().map_err(|err| err.to_string())?;
            {
                let mut stmt = tx
                    .prepare(
                        "INSERT INTO files (
                            path,
                            root_id,
                            file_name,
                            file_name_norm,
                            comment,
                            comment_norm,
                            parent_dir,
                            size_bytes,
                            modified_time,
                            created_time,
                            last_indexed_time
                        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                        ON CONFLICT(path) DO UPDATE SET
                            root_id = excluded.root_id,
                            file_name = excluded.file_name,
                            file_name_norm = excluded.file_name_norm,
                            comment = excluded.comment,
                            comment_norm = excluded.comment_norm,
                            parent_dir = excluded.parent_dir,
                            size_bytes = excluded.size_bytes,
                            modified_time = excluded.modified_time,
                            created_time = excluded.created_time,
                            last_indexed_time = excluded.last_indexed_time",
                    )
                    .map_err(|err| err.to_string())?;

                for file in files {
                    stmt.execute(params![
                        file.path,
                        file.root_id,
                        file.file_name,
                        file.file_name_norm,
                        file.comment,
                        file.comment_norm,
                        file.parent_dir,
                        file.size_bytes,
                        file.modified_time,
                        file.created_time,
                        file.last_indexed_time
                    ])
                    .map_err(|err| err.to_string())?;
                }
            }
            tx.commit().map_err(|err| err.to_string())?;
        }
        WriteCommand::DeletePaths { paths } => {
            if paths.is_empty() {
                return Ok(());
            }
            let tx = conn.transaction().map_err(|err| err.to_string())?;
            {
                let mut stmt = tx
                    .prepare("DELETE FROM files WHERE path = ?")
                    .map_err(|err| err.to_string())?;
                for path in paths {
                    stmt.execute([path.as_str()])
                        .map_err(|err| err.to_string())?;
                }
            }
            tx.commit().map_err(|err| err.to_string())?;
        }
        WriteCommand::DeleteByPrefixes { prefixes } => {
            if prefixes.is_empty() {
                return Ok(());
            }
            let tx = conn.transaction().map_err(|err| err.to_string())?;
            {
                // LIKE の前方一致は OR と ESCAPE のせいで索引が使えず、プレフィックス1件ごとに
                // files を全走査してしまう。主キー path は BINARY 照合なので、配下の判定を
                // 範囲比較へ置き換えて主キー索引で引けるようにする。
                let mut stmt = tx
                    .prepare("DELETE FROM files WHERE path = ? OR (path >= ? AND path < ?)")
                    .map_err(|err| err.to_string())?;
                for prefix in prefixes {
                    let (lower, upper) = descendant_path_range(&prefix);
                    stmt.execute(params![prefix, lower, upper])
                        .map_err(|err| err.to_string())?;
                }
            }
            tx.commit().map_err(|err| err.to_string())?;
        }
        WriteCommand::FinalizeScan {
            root_id,
            marker,
            finished_at,
            resp,
        } => {
            let result = (|| {
                let tx = conn.transaction().map_err(|err| err.to_string())?;
                tx.execute(
                    "DELETE FROM files WHERE root_id = ? AND last_indexed_time < ?",
                    params![root_id, marker],
                )
                .map_err(|err| err.to_string())?;
                tx.execute(
                    "UPDATE roots SET last_scan_time = ? WHERE root_id = ?",
                    params![finished_at, root_id],
                )
                .map_err(|err| err.to_string())?;
                tx.commit().map_err(|err| err.to_string())
            })();
            let _ = resp.send(result.clone());
            result?;
        }
        WriteCommand::Shutdown => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::descendant_path_range;

    fn in_range(path: &str, prefix: &str) -> bool {
        let (lower, upper) = descendant_path_range(prefix);
        path >= lower.as_str() && path < upper.as_str()
    }

    #[test]
    fn range_covers_only_descendants() {
        for (prefix, inside, outside) in [
            (
                "E:\\videos\\live",
                ["E:\\videos\\live\\a.mp4", "E:\\videos\\live\\sub\\b.mp4"],
                ["E:\\videos\\live2\\a.mp4", "E:\\videos\\lit\\a.mp4"],
            ),
            (
                "/Users/vj/videos",
                ["/Users/vj/videos/a.mp4", "/Users/vj/videos/sub/b.mp4"],
                ["/Users/vj/videos2/a.mp4", "/Users/vj/video/a.mp4"],
            ),
        ] {
            for path in inside {
                assert!(in_range(path, prefix), "{path} は {prefix} 配下");
            }
            for path in outside {
                assert!(!in_range(path, prefix), "{path} は {prefix} 配下ではない");
            }
            // ルート自身は範囲に含めず、呼び出し側が path = ? で別途消す。
            assert!(!in_range(prefix, prefix));
        }
    }
}
