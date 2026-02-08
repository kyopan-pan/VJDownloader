use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::paths::{ffmpeg_path, ffprobe_path};

const BUNDLED_FFMPEG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/bin/ffmpeg"));
const BUNDLED_FFPROBE: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/bin/ffprobe"));

pub fn ensure_bundled_tools() -> Result<(), String> {
    ensure_bundled_bin(&ffmpeg_path(), BUNDLED_FFMPEG)?;
    ensure_bundled_bin(&ffprobe_path(), BUNDLED_FFPROBE)?;
    Ok(())
}

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

    let mut perms = fs::metadata(path)
        .map_err(|err| err.to_string())?
        .permissions();
    let mode = perms.mode();
    if mode & 0o111 != 0o111 {
        perms.set_mode(mode | 0o111);
        fs::set_permissions(path, perms).map_err(|err| err.to_string())?;
    }

    Ok(())
}
