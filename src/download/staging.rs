use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fs_utils::ensure_dir;

// ダウンロードごとに衝突しない一時作業フォルダを作成する。
pub(super) fn create_download_staging_dir(output_dir: &Path) -> Result<PathBuf, String> {
    let staging_root = output_dir.join(".vjdownloader-staging");
    ensure_dir(&staging_root).map_err(|err| format!("一時フォルダの準備に失敗しました: {err}"))?;

    let pid = std::process::id();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    for idx in 0..1000u32 {
        let candidate = staging_root.join(format!("job-{timestamp}-{pid}-{idx}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(format!("一時フォルダの作成に失敗しました: {err}")),
        }
    }
    Err("一時フォルダ名の確保に失敗しました。".to_string())
}

// 一時フォルダ内の MP4 のみを最終保存先へ移動する。
pub(super) fn promote_downloaded_mp4_files(
    staging_dir: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    let entries = fs::read_dir(staging_dir)
        .map_err(|err| format!("一時フォルダの読み取りに失敗しました: {err}"))?;
    let mut mp4_files = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|err| format!("一時フォルダの読み取りに失敗しました: {err}"))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_mp4 = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("mp4"))
            .unwrap_or(false);
        if is_mp4 {
            mp4_files.push(path);
        }
    }

    if mp4_files.is_empty() {
        return Err("ダウンロード完了後のMP4ファイルが見つかりませんでした。".to_string());
    }

    mp4_files.sort();
    for src in mp4_files {
        move_file_to_output_dir(&src, output_dir)?;
    }

    Ok(())
}

// 同名衝突を避けながら、最終保存先へファイルを移動する。
fn move_file_to_output_dir(src: &Path, output_dir: &Path) -> Result<(), String> {
    let file_name = src
        .file_name()
        .ok_or_else(|| "保存対象のファイル名が不正です。".to_string())?;
    let mut destination = output_dir.join(file_name);
    if destination.exists() {
        destination = next_available_destination(&destination)?;
    }

    fs::rename(src, &destination).map_err(|err| {
        format!(
            "動画ファイルの配置に失敗しました: {} -> {} ({err})",
            src.to_string_lossy(),
            destination.to_string_lossy()
        )
    })?;

    Ok(())
}

// 既存ファイルがある場合、"(n)" サフィックス付きの保存先を探す。
fn next_available_destination(base_path: &Path) -> Result<PathBuf, String> {
    let parent = base_path
        .parent()
        .ok_or_else(|| "保存先フォルダの解決に失敗しました。".to_string())?;
    let stem = base_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "video".to_string());
    let ext = base_path
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    for idx in 1..=9999u32 {
        let file_name = if ext.is_empty() {
            format!("{stem} ({idx})")
        } else {
            format!("{stem} ({idx}).{ext}")
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("同名ファイルが多すぎるため保存先を確保できませんでした。".to_string())
}
