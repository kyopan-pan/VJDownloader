use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use super::db::{apply_migrations, open_connection};
use super::normalize::escape_like_pattern;
use super::{EngineResult, WriteCommand};

// 書き込み専用スレッドでコマンドを順次適用する。
pub(super) fn writer_loop(db_path: PathBuf, rx: Receiver<WriteCommand>) {
    let mut conn = match open_connection(&db_path).and_then(|conn| {
        apply_migrations(&conn)?;
        Ok(conn)
    }) {
        Ok(conn) => conn,
        Err(err) => {
            eprintln!("[search-index] writer failed to initialize DB: {err}");
            return;
        }
    };

    while let Ok(cmd) = rx.recv() {
        if let WriteCommand::Shutdown = cmd {
            break;
        }

        if let Err(err) = apply_write_command(&mut conn, cmd) {
            eprintln!("[search-index] writer command failed: {err}");
        }
    }
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
                            parent_dir,
                            size_bytes,
                            modified_time,
                            created_time,
                            last_indexed_time
                        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                        ON CONFLICT(path) DO UPDATE SET
                            root_id = excluded.root_id,
                            file_name = excluded.file_name,
                            file_name_norm = excluded.file_name_norm,
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
                let mut stmt = tx
                    .prepare("DELETE FROM files WHERE path = ? OR path LIKE ? ESCAPE '\\'")
                    .map_err(|err| err.to_string())?;
                for prefix in prefixes {
                    let sep = if prefix.contains('\\') { '\\' } else { '/' };
                    let escaped = escape_like_pattern(&prefix);
                    let pattern = format!("{escaped}{sep}%");
                    stmt.execute(params![prefix, pattern])
                        .map_err(|err| err.to_string())?;
                }
            }
            tx.commit().map_err(|err| err.to_string())?;
        }
        WriteCommand::FinalizeScan {
            root_id,
            marker,
            finished_at,
        } => {
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
            tx.commit().map_err(|err| err.to_string())?;
        }
        WriteCommand::Shutdown => {}
    }
    Ok(())
}
