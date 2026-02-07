use std::fs;
use std::path::{Path, PathBuf};

pub fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| err.to_string())
}

pub fn load_mp4_files(dir: &Path) -> Vec<PathBuf> {
    let _ = ensure_dir(dir);
    let mut items: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext != "mp4" {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        items.push((path, modified));
    }

    items.sort_by(|a, b| b.1.cmp(&a.1));
    items.into_iter().map(|(path, _)| path).collect()
}

pub fn delete_download_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("ファイルが見つかりません。".to_string());
    }
    fs::remove_file(path).map_err(|err| err.to_string())
}
