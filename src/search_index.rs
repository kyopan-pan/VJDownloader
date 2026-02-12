use notify::event::ModifyKind;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;
use walkdir::WalkDir;

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

    pub fn reindex_all_async(&self) -> EngineResult<()> {
        let roots = self.list_roots()?;
        for root in roots.into_iter().filter(|root| root.is_enabled) {
            self.start_full_scan(root.root_id, PathBuf::from(root.root_path));
        }
        Ok(())
    }

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
        old_path: Option<&Path>,
        new_path: Option<&Path>,
    ) -> EngineResult<()> {
        let roots = self.enabled_watched_roots()?;
        if let Some(old) = old_path {
            apply_delete_change(old, &roots, &self.inner.write_tx)?;
        }
        if let Some(new_path) = new_path {
            apply_upsert_change(new_path, &roots, &self.inner.write_tx)?;
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

#[derive(Clone)]
enum QueryPattern {
    Prefix {
        pattern: String,
        exact: String,
    },
    Contains {
        pattern: String,
        prefix_pattern: String,
    },
}

fn run_search_query(
    conn: &Connection,
    request: &SearchRequest,
    pattern: Option<QueryPattern>,
    limit: usize,
) -> EngineResult<Vec<SearchHit>> {
    let mut sql = String::from(
        "SELECT f.path, f.file_name, f.size_bytes, f.modified_time, f.root_id, f.parent_dir
         FROM files f
         JOIN roots r ON r.root_id = f.root_id
         WHERE r.is_enabled = 1",
    );
    let mut params = Vec::<Value>::new();

    if let Some(root_id) = request.root_id {
        sql.push_str(" AND f.root_id = ?");
        params.push(Value::from(root_id));
    }

    if let Some(root_path) = request
        .root_path
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        let normalized = normalize_root_path(Path::new(root_path))?;
        sql.push_str(" AND r.root_path = ?");
        params.push(Value::from(path_to_key(&normalized)));
    }

    if let Some(parent_dir) = request
        .parent_dir
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        let normalized_parent = normalize_parent_for_filter(parent_dir);
        sql.push_str(" AND f.parent_dir = ?");
        params.push(Value::from(normalized_parent));
    }

    if let Some(modified_after) = request.modified_after {
        sql.push_str(" AND f.modified_time >= ?");
        params.push(Value::from(modified_after));
    }

    if let Some(modified_before) = request.modified_before {
        sql.push_str(" AND f.modified_time <= ?");
        params.push(Value::from(modified_before));
    }

    if let Some(size_min) = request.size_min {
        sql.push_str(" AND f.size_bytes >= ?");
        params.push(Value::from(size_min));
    }

    if let Some(size_max) = request.size_max {
        sql.push_str(" AND f.size_bytes <= ?");
        params.push(Value::from(size_max));
    }

    match pattern {
        Some(QueryPattern::Prefix { pattern, exact }) => {
            sql.push_str(" AND f.file_name_norm LIKE ? ESCAPE '\\'");
            params.push(Value::from(pattern.clone()));
            sql.push_str(" ORDER BY CASE WHEN f.file_name_norm = ? THEN 0 ELSE 1 END ASC,");
            params.push(Value::from(exact));
            push_sort_clause(&mut sql, request.sort);
        }
        Some(QueryPattern::Contains {
            pattern,
            prefix_pattern,
        }) => {
            sql.push_str(" AND f.file_name_norm LIKE ? ESCAPE '\\'");
            params.push(Value::from(pattern));
            sql.push_str(" AND f.file_name_norm NOT LIKE ? ESCAPE '\\'");
            params.push(Value::from(prefix_pattern));
            sql.push_str(" ORDER BY ");
            push_sort_clause(&mut sql, request.sort);
        }
        None => {
            sql.push_str(" ORDER BY ");
            push_sort_clause(&mut sql, request.sort);
        }
    }

    sql.push_str(" LIMIT ?");
    params.push(Value::from(limit as i64));

    let mut stmt = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |row| {
            Ok(SearchHit {
                path: row.get(0)?,
                file_name: row.get(1)?,
                size_bytes: row.get(2)?,
                modified_time: row.get(3)?,
                root_id: row.get(4)?,
                parent_dir: row.get(5)?,
            })
        })
        .map_err(|err| err.to_string())?;

    let mut hits = Vec::new();
    for row in rows {
        hits.push(row.map_err(|err| err.to_string())?);
    }
    Ok(hits)
}

fn push_sort_clause(sql: &mut String, sort: SearchSort) {
    match sort {
        SearchSort::ModifiedDesc => {
            sql.push_str(" f.modified_time DESC, f.file_name_norm ASC");
        }
        SearchSort::NameAsc => {
            sql.push_str(" f.file_name_norm ASC, f.modified_time DESC");
        }
    }
}

fn writer_loop(db_path: PathBuf, rx: Receiver<WriteCommand>) {
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

fn apply_write_command(conn: &mut Connection, cmd: WriteCommand) -> EngineResult<()> {
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

fn watcher_loop(rx: Receiver<WatcherMessage>, write_tx: Sender<WriteCommand>, db_path: PathBuf) {
    let (event_tx, event_rx) = mpsc::channel();
    let callback_tx = event_tx.clone();
    let mut watcher = match RecommendedWatcher::new(
        move |res| {
            let _ = callback_tx.send(res);
        },
        Config::default(),
    ) {
        Ok(watcher) => watcher,
        Err(err) => {
            eprintln!("[search-index] failed to create watcher: {err}");
            return;
        }
    };

    let mut watched_roots = Vec::<WatchedRoot>::new();
    let mut pending = PendingChanges::default();

    loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WatcherMessage::SetRoots(roots) => {
                    reset_watch_targets(&mut watcher, &mut watched_roots, roots);
                }
                WatcherMessage::Shutdown => return,
            }
        }

        match event_rx.recv_timeout(Duration::from_millis(150)) {
            Ok(Ok(event)) => {
                collect_pending_change(&mut pending, &event);
            }
            Ok(Err(err)) => {
                eprintln!("[search-index] watcher event error: {err}");
                trigger_reindex_all_from_db(&db_path, &write_tx);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        if should_flush_pending(&pending) {
            if let Err(err) = flush_pending_changes(&mut pending, &watched_roots, &write_tx) {
                eprintln!("[search-index] failed to flush watcher changes: {err}");
                trigger_reindex_all_from_db(&db_path, &write_tx);
            }
        }
    }
}

fn reset_watch_targets(
    watcher: &mut RecommendedWatcher,
    current: &mut Vec<WatchedRoot>,
    next: Vec<WatchedRoot>,
) {
    for root in current.iter() {
        if let Err(err) = watcher.unwatch(&root.root_path) {
            eprintln!(
                "[search-index] failed to unwatch {}: {}",
                root.root_path.to_string_lossy(),
                err
            );
        }
    }

    current.clear();
    for root in next {
        if !root.root_path.exists() {
            continue;
        }
        if let Err(err) = watcher.watch(&root.root_path, RecursiveMode::Recursive) {
            eprintln!(
                "[search-index] failed to watch {}: {}",
                root.root_path.to_string_lossy(),
                err
            );
            continue;
        }
        current.push(root);
    }
}

fn collect_pending_change(pending: &mut PendingChanges, event: &Event) {
    if matches!(event.kind, EventKind::Modify(ModifyKind::Name(_))) && event.paths.len() >= 2 {
        pending
            .moves
            .push((event.paths[0].clone(), event.paths[1].clone()));
        pending.last_change_at = Some(Instant::now());
        return;
    }

    for path in &event.paths {
        pending.path_changes.insert(path.clone());
    }
    pending.last_change_at = Some(Instant::now());
}

fn should_flush_pending(pending: &PendingChanges) -> bool {
    if pending.path_changes.is_empty() && pending.moves.is_empty() {
        return false;
    }

    pending
        .last_change_at
        .map(|last| last.elapsed() >= DEBOUNCE_WINDOW)
        .unwrap_or(false)
}

fn flush_pending_changes(
    pending: &mut PendingChanges,
    roots: &[WatchedRoot],
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    let mut delete_paths = HashSet::<String>::new();
    let mut delete_prefixes = HashSet::<String>::new();
    let mut upsert_paths = HashSet::<PathBuf>::new();

    for (old_path, new_path) in pending.moves.drain(..) {
        collect_delete_target(&old_path, &mut delete_paths, &mut delete_prefixes);
        upsert_paths.insert(new_path);
    }

    for path in pending.path_changes.drain() {
        upsert_paths.insert(path);
    }

    pending.last_change_at = None;

    for path in upsert_paths {
        if path.exists() {
            let metadata = match fs::metadata(&path) {
                Ok(meta) => meta,
                Err(_) => {
                    continue;
                }
            };

            if metadata.is_dir() {
                upsert_directory(&path, roots, write_tx)?;
                continue;
            }

            if !is_mp4_path(&path) {
                continue;
            }

            if let Some(root_id) = find_root_id_for_path(&path, roots) {
                if let Some(record) = build_record_from_path(root_id, &path, epoch_millis()) {
                    write_tx
                        .send(WriteCommand::UpsertFiles {
                            files: vec![record],
                        })
                        .map_err(|err| err.to_string())?;
                }
            }
        } else {
            collect_delete_target(&path, &mut delete_paths, &mut delete_prefixes);
        }
    }

    if !delete_paths.is_empty() {
        write_tx
            .send(WriteCommand::DeletePaths {
                paths: delete_paths.into_iter().collect(),
            })
            .map_err(|err| err.to_string())?;
    }

    if !delete_prefixes.is_empty() {
        write_tx
            .send(WriteCommand::DeleteByPrefixes {
                prefixes: delete_prefixes.into_iter().collect(),
            })
            .map_err(|err| err.to_string())?;
    }

    Ok(())
}

fn collect_delete_target(
    path: &Path,
    delete_paths: &mut HashSet<String>,
    delete_prefixes: &mut HashSet<String>,
) {
    let key = path_to_key(path);
    if path.exists() && path.is_dir() {
        delete_prefixes.insert(key);
        return;
    }

    if path.exists() {
        delete_paths.insert(key);
        return;
    }

    // A disappeared path can be either a file or a directory (e.g. rename old-path),
    // so delete both exact path and descendant paths to avoid stale index entries.
    delete_paths.insert(key.clone());
    delete_prefixes.insert(key);
}

fn upsert_directory(
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

fn trigger_reindex_all_from_db(db_path: &Path, write_tx: &Sender<WriteCommand>) {
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

#[cfg(test)]
fn apply_delete_change(
    old_path: &Path,
    _roots: &[WatchedRoot],
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    if old_path.is_dir() {
        write_tx
            .send(WriteCommand::DeleteByPrefixes {
                prefixes: vec![path_to_key(old_path)],
            })
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    write_tx
        .send(WriteCommand::DeletePaths {
            paths: vec![path_to_key(old_path)],
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

#[cfg(test)]
fn apply_upsert_change(
    new_path: &Path,
    roots: &[WatchedRoot],
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    if !new_path.exists() {
        return Ok(());
    }

    let metadata = fs::metadata(new_path).map_err(|err| err.to_string())?;
    if metadata.is_dir() {
        return upsert_directory(new_path, roots, write_tx);
    }

    if !is_mp4_path(new_path) {
        return Ok(());
    }

    let Some(root_id) = find_root_id_for_path(new_path, roots) else {
        return Ok(());
    };

    if let Some(record) = build_record_from_path(root_id, new_path, epoch_millis()) {
        write_tx
            .send(WriteCommand::UpsertFiles {
                files: vec![record],
            })
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn find_root_id_for_path(path: &Path, roots: &[WatchedRoot]) -> Option<i64> {
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

fn scan_root(root_id: i64, root_path: &Path, write_tx: &Sender<WriteCommand>) -> EngineResult<()> {
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

fn build_record_from_path(root_id: i64, path: &Path, marker: i64) -> Option<FileRecord> {
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

fn open_connection(path: &Path) -> EngineResult<Connection> {
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

fn apply_migrations(conn: &Connection) -> EngineResult<()> {
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

fn normalize_root_path(path: &Path) -> EngineResult<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .map_err(|err| err.to_string())
        .map(|current| current.join(path))
}

fn normalize_parent_for_filter(raw: &str) -> String {
    let path = PathBuf::from(raw.trim());
    if path.is_absolute() {
        return path_to_key(&path);
    }

    path_to_key(
        &std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path),
    )
}

fn normalize_for_search(input: &str) -> String {
    input.trim().nfkc().collect::<String>().to_lowercase()
}

fn normalize_query(query: &str) -> String {
    normalize_for_search(query)
}

fn escape_like_pattern(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            _ => out.push(ch),
        }
    }
    out
}

fn path_to_key(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn is_mp4_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("mp4"))
        .unwrap_or(false)
}

fn system_time_to_epoch_secs(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn epoch_secs() -> i64 {
    system_time_to_epoch_secs(SystemTime::now())
}

fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_dummy(path: &Path, bytes: usize) {
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
