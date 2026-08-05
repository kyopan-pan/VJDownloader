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

    if version < 2 {
        conn.execute_batch(
            "BEGIN;
            ALTER TABLE files ADD COLUMN comment TEXT NOT NULL DEFAULT '';
            ALTER TABLE files ADD COLUMN comment_norm TEXT;
            PRAGMA user_version = 2;
            COMMIT;",
        )
        .map_err(|err| err.to_string())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::apply_migrations;

    #[test]
    fn migrates_existing_index_to_comment_columns() {
        let conn = Connection::open_in_memory().expect("open DB");
        conn.execute_batch(
            "CREATE TABLE roots (
                root_id INTEGER PRIMARY KEY,
                root_path TEXT NOT NULL UNIQUE,
                is_enabled INTEGER NOT NULL DEFAULT 1,
                last_scan_time INTEGER
            );
            CREATE TABLE files (
                path TEXT PRIMARY KEY,
                root_id INTEGER NOT NULL,
                file_name TEXT NOT NULL,
                file_name_norm TEXT NOT NULL,
                parent_dir TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                modified_time INTEGER NOT NULL,
                created_time INTEGER,
                last_indexed_time INTEGER NOT NULL
            );
            PRAGMA user_version = 1;",
        )
        .expect("create version 1 schema");

        apply_migrations(&conn).expect("migrate schema");

        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read schema version");
        assert_eq!(version, 2);

        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('files')")
            .expect("prepare column query");
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query columns")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect columns");
        assert!(columns.iter().any(|column| column == "comment"));
        assert!(columns.iter().any(|column| column == "comment_norm"));
    }
}
