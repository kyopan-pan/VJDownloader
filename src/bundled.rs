#[cfg(not(target_os = "windows"))]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(not(target_os = "windows"))]
use std::path::Path;

#[cfg(target_os = "windows")]
use crate::paths::media_tools_ready;
#[cfg(not(target_os = "windows"))]
use crate::paths::{ffmpeg_path, ffprobe_path};

#[cfg(not(target_os = "windows"))]
const BUNDLED_FFMPEG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/bin/ffmpeg"));
#[cfg(not(target_os = "windows"))]
const BUNDLED_FFPROBE: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/bin/ffprobe"));

#[cfg(not(target_os = "windows"))]
pub fn ensure_bundled_tools() -> Result<(), String> {
    ensure_bundled_bin(&ffmpeg_path(), BUNDLED_FFMPEG)?;
    ensure_bundled_bin(&ffprobe_path(), BUNDLED_FFPROBE)?;
    Ok(())
}

// Windowsはffmpeg/ffprobeを同梱せず初回セットアップで取得するため、ここでは状態確認だけを行う。
// 取得自体はUIを止めないよう、起動時のバックグラウンド処理とダウンロード開始時に実施する。
#[cfg(target_os = "windows")]
pub fn ensure_bundled_tools() -> Result<(), String> {
    if media_tools_ready() {
        Ok(())
    } else {
        Err("ffmpeg/ffprobeが見つかりません。セットアップの完了を待つか、設定から再実行してください。".to_string())
    }
}

#[cfg(not(target_os = "windows"))]
fn ensure_bundled_bin(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }

    let needs_write = match fs::metadata(path) {
        Ok(meta) => meta.len() != bytes.len() as u64,
        Err(_) => true,
    };

    if needs_write {
        fs::write(path, bytes).map_err(|err| err.to_string())?;
    }

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(path)
            .map_err(|err| err.to_string())?
            .permissions();
        let mode = perms.mode();
        if mode & 0o111 != 0o111 {
            perms.set_mode(mode | 0o111);
            fs::set_permissions(path, perms).map_err(|err| err.to_string())?;
        }
    }

    Ok(())
}
