use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

use super::{DB_SCHEMA_VERSION, EngineResult};

// SQLite 接続を開き、検索用途向け PRAGMA を適用する。
pub(super) fn open_connection(path: &Path) -> EngineResult<Connection> {
    let conn = Connection::open(path).map_err(|err| err.to_string())?;
    conn.busy_timeout(Duration::from_millis(2_000))
        .map_err(|err| err.to_string())?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|err| err.to_string())?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|err| err.to_string())?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|err| err.to_string())?;
    Ok(conn)
}

// スキーマバージョンを確認し、必要な初期テーブル/インデックスを作成する。
pub(super) fn apply_migrations(conn: &Connection) -> EngineResult<()> {
    let version: i32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|err| err.to_string())?;

    if version > DB_SCHEMA_VERSION {
        return Err(format!(
            "DB schema version {version} is newer than supported version {DB_SCHEMA_VERSION}"
        ));
    }

    if version == 0 {
        conn.execute_batch(
            "BEGIN;
            CREATE TABLE IF NOT EXISTS roots (
                root_id INTEGER PRIMARY KEY AUTOINCREMENT,
                root_path TEXT NOT NULL UNIQUE,
                is_enabled INTEGER NOT NULL DEFAULT 1,
                last_scan_time INTEGER
            );

            CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY,
                root_id INTEGER NOT NULL,
                file_name TEXT NOT NULL,
                file_name_norm TEXT NOT NULL,
                parent_dir TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                modified_time INTEGER NOT NULL,
                created_time INTEGER,
                last_indexed_time INTEGER NOT NULL,
                FOREIGN KEY(root_id) REFERENCES roots(root_id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_files_root_id ON files(root_id);
            CREATE INDEX IF NOT EXISTS idx_files_parent_dir ON files(parent_dir);
            CREATE INDEX IF NOT EXISTS idx_files_file_name_norm ON files(file_name_norm);
            CREATE INDEX IF NOT EXISTS idx_files_modified_time ON files(modified_time);
            CREATE INDEX IF NOT EXISTS idx_files_size_bytes ON files(size_bytes);

            PRAGMA user_version = 1;
            COMMIT;",
        )
        .map_err(|err| err.to_string())?;
    }

    Ok(())
}
