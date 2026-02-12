use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use std::path::Path;

use super::normalize::{normalize_parent_for_filter, normalize_root_path, path_to_key};
use super::{EngineResult, SearchHit, SearchRequest, SearchSort};

#[derive(Clone)]
pub(super) enum QueryPattern {
    Prefix {
        pattern: String,
        exact: String,
    },
    Contains {
        pattern: String,
        prefix_pattern: String,
    },
}

// 検索条件を SQL に組み立て、files テーブルからヒットを取得する。
pub(super) fn run_search_query(
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

// ソート種別に応じて ORDER BY 句を追加する。
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
