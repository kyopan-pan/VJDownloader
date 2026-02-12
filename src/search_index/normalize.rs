use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

use super::EngineResult;

// ルートパスを絶対パスへ正規化する。
pub(super) fn normalize_root_path(path: &Path) -> EngineResult<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .map_err(|err| err.to_string())
        .map(|current| current.join(path))
}

// 親ディレクトリ絞り込み用の文字列を検索キー形式へ正規化する。
pub(super) fn normalize_parent_for_filter(raw: &str) -> String {
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

// 文字列検索のために Unicode 正規化と小文字化を行う。
pub(super) fn normalize_for_search(input: &str) -> String {
    input.trim().nfkc().collect::<String>().to_lowercase()
}

// 検索クエリを index 正規化ルールへ合わせる。
pub(super) fn normalize_query(query: &str) -> String {
    normalize_for_search(query)
}

// SQL LIKE で意味を持つ文字をエスケープする。
pub(super) fn escape_like_pattern(input: &str) -> String {
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

// Path を DB 主キー比較で使う文字列表現に変換する。
pub(super) fn path_to_key(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

// MP4 ファイルかどうかを拡張子で判定する。
pub(super) fn is_mp4_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("mp4"))
        .unwrap_or(false)
}

// SystemTime を UNIX 秒へ変換する。
pub(super) fn system_time_to_epoch_secs(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

// 現在時刻を UNIX 秒で返す。
pub(super) fn epoch_secs() -> i64 {
    system_time_to_epoch_secs(SystemTime::now())
}

// 現在時刻を UNIX ミリ秒で返す。
pub(super) fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
