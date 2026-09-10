use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use super::db::{apply_migrations, open_connection};
use super::{CommentRecord, EngineResult, WriteCommand};

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

// フェーズ2（コメント補完）の書き込み。
fn upsert_comments(conn: &mut Connection, comments: Vec<CommentRecord>) -> EngineResult<()> {
    if comments.is_empty() {
        return Ok(());
    }

    let tx = conn.transaction().map_err(|err| err.to_string())?;
    {
        // ffprobe 実行中に監視側が同じ行を更新していることがあるため、読み出した時点の
        // サイズと更新日時が一致する行だけを更新して、古い内容の上書きを防ぐ。
        let mut stmt = tx
            .prepare(
                "UPDATE files
                 SET comment = ?, comment_norm = ?
                 WHERE path = ? AND size_bytes = ? AND modified_time = ?",
            )
            .map_err(|err| err.to_string())?;

        for comment in comments {
            stmt.execute(params![
                comment.comment,
                comment.comment_norm,
                comment.path,
                comment.size_bytes,
                comment.modified_time
            ])
            .map_err(|err| err.to_string())?;
        }
    }
    tx.commit().map_err(|err| err.to_string())
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
                // フェーズ1（ファイル名インデックス）の書き込み。コメント列は触らない。
                // サイズと更新日時が変わっていない行は取得済みコメントをそのまま残し、
                // 変わっていれば NULL に戻してフェーズ2の取得待ちへ入れる。
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
                            last_indexed_time,
                            comment,
                            comment_norm
                        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, '', NULL)
                        ON CONFLICT(path) DO UPDATE SET
                            root_id = excluded.root_id,
                            file_name = excluded.file_name,
                            file_name_norm = excluded.file_name_norm,
                            parent_dir = excluded.parent_dir,
                            comment = CASE
                                WHEN files.size_bytes = excluded.size_bytes
                                     AND files.modified_time = excluded.modified_time
                                THEN files.comment
                                ELSE ''
                            END,
                            comment_norm = CASE
                                WHEN files.size_bytes = excluded.size_bytes
                                     AND files.modified_time = excluded.modified_time
                                THEN files.comment_norm
                                ELSE NULL
                            END,
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
        WriteCommand::UpsertComments { comments, resp } => {
            let result = upsert_comments(conn, comments);
            let _ = resp.send(result.clone());
            result?;
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
    use rusqlite::Connection;
    use std::sync::mpsc;

    use super::super::db::apply_migrations;
    use super::super::{CommentRecord, FileRecord, WriteCommand};
    use super::{apply_write_command, descendant_path_range};

    fn file_record(size_bytes: i64, modified_time: i64) -> FileRecord {
        FileRecord {
            path: "/videos/a.mp4".to_string(),
            root_id: 1,
            file_name: "a.mp4".to_string(),
            file_name_norm: "a.mp4".to_string(),
            parent_dir: "/videos".to_string(),
            size_bytes,
            modified_time,
            created_time: None,
            last_indexed_time: 1,
        }
    }

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open DB");
        apply_migrations(&conn).expect("migrate");
        conn.execute("INSERT INTO roots (root_path) VALUES ('/videos')", [])
            .expect("insert root");
        conn
    }

    fn stored_comment(conn: &Connection) -> (String, Option<String>) {
        conn.query_row(
            "SELECT comment, comment_norm FROM files WHERE path = '/videos/a.mp4'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read comment")
    }

    fn upsert_comment(conn: &mut Connection, size_bytes: i64, modified_time: i64) {
        let (tx, rx) = mpsc::channel();
        apply_write_command(
            conn,
            WriteCommand::UpsertComments {
                comments: vec![CommentRecord {
                    path: "/videos/a.mp4".to_string(),
                    size_bytes,
                    modified_time,
                    comment: "概要欄".to_string(),
                    comment_norm: "概要欄".to_string(),
                }],
                resp: tx,
            },
        )
        .expect("upsert comments");
        rx.recv().expect("response").expect("commit");
    }

    fn upsert_file(conn: &mut Connection, size_bytes: i64, modified_time: i64) {
        apply_write_command(
            conn,
            WriteCommand::UpsertFiles {
                files: vec![file_record(size_bytes, modified_time)],
            },
        )
        .expect("upsert files");
    }

    #[test]
    fn phase1_upsert_leaves_new_rows_waiting_for_comments() {
        let mut conn = setup_conn();
        upsert_file(&mut conn, 100, 10);
        assert_eq!(stored_comment(&conn), (String::new(), None));
    }

    #[test]
    fn phase1_upsert_keeps_comments_of_unchanged_files() {
        let mut conn = setup_conn();
        upsert_file(&mut conn, 100, 10);
        upsert_comment(&mut conn, 100, 10);

        // 実体が変わっていない再走査ではコメントを取り直させない。
        upsert_file(&mut conn, 100, 10);
        assert_eq!(
            stored_comment(&conn),
            ("概要欄".to_string(), Some("概要欄".to_string()))
        );
    }

    #[test]
    fn phase1_upsert_clears_comments_of_changed_files() {
        let mut conn = setup_conn();
        upsert_file(&mut conn, 100, 10);
        upsert_comment(&mut conn, 100, 10);

        // サイズが変わったら取得し直させる。
        upsert_file(&mut conn, 200, 10);
        assert_eq!(stored_comment(&conn), (String::new(), None));

        upsert_comment(&mut conn, 200, 10);
        // 更新日時が変わった場合も同じ。
        upsert_file(&mut conn, 200, 20);
        assert_eq!(stored_comment(&conn), (String::new(), None));
    }

    #[test]
    fn comment_upsert_skips_rows_that_changed_while_probing() {
        let mut conn = setup_conn();
        upsert_file(&mut conn, 100, 10);
        // ffprobe 実行中に実体が差し替わった想定。読み出し時点の値では更新させない。
        upsert_comment(&mut conn, 999, 999);
        assert_eq!(stored_comment(&conn), (String::new(), None));
    }

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
