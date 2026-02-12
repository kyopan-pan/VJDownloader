use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

use crate::fs_utils::ensure_dir;
use crate::paths::{bin_dir, deno_path, yt_dlp_path};

use super::DownloadEvent;

// yt-dlp が存在しない場合は取得し、実行権限を保証して返す。
pub fn ensure_yt_dlp(tx: Option<&mpsc::Sender<DownloadEvent>>) -> Result<PathBuf, String> {
    let yt_dlp = yt_dlp_path();
    if yt_dlp.exists() {
        ensure_executable(&yt_dlp)?;
        return Ok(yt_dlp);
    }

    let bin = bin_dir();
    ensure_dir(&bin)?;
    if let Some(tx) = tx {
        let _ = tx.send(DownloadEvent::Log(
            "yt-dlpが見つかりません。ダウンロードします。".to_string(),
        ));
    }

    let url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos";
    curl_download(url, &yt_dlp, "yt-dlp")?;

    ensure_executable(&yt_dlp)?;
    if let Some(tx) = tx {
        let _ = tx.send(DownloadEvent::Log(
            "yt-dlpをダウンロードしました。".to_string(),
        ));
    }
    Ok(yt_dlp)
}

// deno が存在しない場合は ZIP 取得と展開を行い、実行権限を保証して返す。
pub fn ensure_deno(tx: Option<&mpsc::Sender<DownloadEvent>>) -> Result<PathBuf, String> {
    let deno = deno_path();
    if deno.exists() {
        ensure_executable(&deno)?;
        return Ok(deno);
    }

    let bin = bin_dir();
    ensure_dir(&bin)?;
    if let Some(tx) = tx {
        let _ = tx.send(DownloadEvent::Log(
            "denoが見つかりません。ダウンロードします。".to_string(),
        ));
    }

    let zip_path = bin.join("deno.zip");
    let url =
        "https://github.com/denoland/deno/releases/latest/download/deno-aarch64-apple-darwin.zip";
    curl_download(url, &zip_path, "deno")?;

    let status = Command::new("unzip")
        .arg("-o")
        .arg(zip_path.to_string_lossy().to_string())
        .arg("-d")
        .arg(bin.to_string_lossy().to_string())
        .status()
        .map_err(|err| format!("unzip起動に失敗しました: {err}"))?;

    let _ = fs::remove_file(&zip_path);

    if !status.success() {
        return Err(format!("denoの展開に失敗しました: {status}"));
    }

    if !deno.exists() {
        return Err("denoが見つかりません。".to_string());
    }

    ensure_executable(&deno)?;
    if let Some(tx) = tx {
        let _ = tx.send(DownloadEvent::Log(
            "denoをダウンロードしました。".to_string(),
        ));
    }
    Ok(deno)
}

// 既存バイナリをバックアップしてから更新し、失敗時はロールバックする。
pub fn update_yt_dlp(tx: Option<&mpsc::Sender<DownloadEvent>>) -> Result<PathBuf, String> {
    let yt_dlp = yt_dlp_path();
    update_tool_with_rollback(&yt_dlp, "yt-dlp", tx, ensure_yt_dlp)
}

// 既存バイナリをバックアップしてから更新し、失敗時はロールバックする。
pub fn update_deno(tx: Option<&mpsc::Sender<DownloadEvent>>) -> Result<PathBuf, String> {
    let deno = deno_path();
    update_tool_with_rollback(&deno, "deno", tx, ensure_deno)
}

// yt-dlp の通常ダウンロード用引数セットを組み立てる。
pub(super) fn base_yt_dlp_args(ffmpeg_path: &str, cookie_args: &[String]) -> Vec<String> {
    let mut args = vec!["--no-playlist".to_string()];
    args.extend(cookie_args.iter().cloned());
    args.extend(vec![
        "--extractor-args".to_string(),
        "youtube:player_client=web".to_string(),
        "--extractor-args".to_string(),
        "youtube:skip=translated_subs".to_string(),
        "--concurrent-fragments".to_string(),
        "4".to_string(),
        "-S".to_string(),
        "vcodec:h264,res,acodec:m4a".to_string(),
        "--match-filter".to_string(),
        "vcodec~='(?i)^(avc|h264)'".to_string(),
    ]);

    args.push("--merge-output-format".to_string());
    args.push("mp4".to_string());
    args.push("--ffmpeg-location".to_string());
    args.push(ffmpeg_path.to_string());
    args.push("--js-runtimes".to_string());
    args.push("deno".to_string());

    args
}

// H.264 優先モードが失敗した場合のフォールバック引数セットを組み立てる。
pub(super) fn fallback_yt_dlp_args(ffmpeg_path: &str, cookie_args: &[String]) -> Vec<String> {
    let mut args = vec!["--no-playlist".to_string()];
    args.extend(cookie_args.iter().cloned());
    args.extend(vec![
        "--extractor-args".to_string(),
        "youtube:player_client=web".to_string(),
        "--extractor-args".to_string(),
        "youtube:skip=translated_subs".to_string(),
        "--concurrent-fragments".to_string(),
        "4".to_string(),
    ]);

    args.push("-f".to_string());
    args.push("bv*[height<=720]+ba/b[height<=720]".to_string());
    args.push("--recode-video".to_string());
    args.push("mp4".to_string());
    args.push("--postprocessor-args".to_string());
    args.push("VideoConvertor:-c:v h264_videotoolbox -b:v 5M -pix_fmt yuv420p".to_string());
    args.push("--ffmpeg-location".to_string());
    args.push(ffmpeg_path.to_string());
    args.push("--js-runtimes".to_string());
    args.push("deno".to_string());

    args
}

fn update_tool_with_rollback<F>(
    path: &Path,
    label: &str,
    tx: Option<&mpsc::Sender<DownloadEvent>>,
    installer: F,
) -> Result<PathBuf, String>
where
    F: FnOnce(Option<&mpsc::Sender<DownloadEvent>>) -> Result<PathBuf, String>,
{
    if !path.exists() {
        return installer(tx);
    }

    let backup_path = next_backup_path(path);
    fs::rename(path, &backup_path)
        .map_err(|err| format!("{label}の更新準備に失敗しました: {err}"))?;

    match installer(tx) {
        Ok(updated_path) => {
            let _ = fs::remove_file(&backup_path);
            Ok(updated_path)
        }
        Err(err) => {
            if path.exists() {
                let _ = fs::remove_file(path);
            }
            match fs::rename(&backup_path, path) {
                Ok(()) => Err(err),
                Err(restore_err) => Err(format!(
                    "{label}の更新に失敗し、旧バージョンの復元にも失敗しました: {restore_err} (更新エラー: {err})"
                )),
            }
        }
    }
}

fn next_backup_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tool");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();

    for idx in 0..1000 {
        let suffix = if idx == 0 {
            format!("{pid}")
        } else {
            format!("{pid}.{idx}")
        };
        let candidate = parent.join(format!("{file_name}.update-backup.{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
    }

    parent.join(format!("{file_name}.update-backup.fallback"))
}

fn ensure_executable(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|err| err.to_string())?;
    let mut perms = metadata.permissions();
    let mode = perms.mode();
    if mode & 0o111 != 0o111 {
        perms.set_mode(mode | 0o111);
        fs::set_permissions(path, perms).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn curl_download(url: &str, output_path: &Path, label: &str) -> Result<(), String> {
    let status = Command::new("curl")
        .arg("-L")
        .arg("-o")
        .arg(output_path.to_string_lossy().to_string())
        .arg(url)
        .status()
        .map_err(|err| format!("curl起動に失敗しました: {err}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{label}のダウンロードに失敗しました: {status}"))
    }
}
