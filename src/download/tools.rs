use std::fs;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::converter::h264_encoder;
use crate::fs_utils::{ensure_dir, is_executable};
use crate::paths::{bin_dir, deno_path, executable_name, yt_dlp_path};
use crate::platform::process::hidden_command;

use super::DownloadMode;

// 途中で切れたダウンロードを壊れたバイナリとして扱うための下限サイズ。
const MIN_TOOL_BYTES: u64 = 1024 * 1024;
// Windows向けffmpeg/ffprobeの公式ビルド配布元。1つのZIPに両方が含まれる静的ビルドを使う。
#[cfg(target_os = "windows")]
const FFMPEG_WINDOWS_X64_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";
#[cfg(target_os = "windows")]
const FFMPEG_WINDOWS_ARM64_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-winarm64-gpl.zip";
// 起動直後の取得とダウンロード開始時の取得が重なって二重にDLしないよう直列化する。
#[cfg(target_os = "windows")]
static FFMPEG_INSTALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
// 一時作業フォルダの名前に付ける共通プレフィックス。
const STAGING_PREFIX: &str = ".tool-staging-";
// 別プロセスが使用中の一時フォルダを削除しないため、十分に古いものだけを掃除する。
const STALE_STAGING_AGE: Duration = Duration::from_secs(24 * 60 * 60);
// 置き換え時に旧バージョンを一時退避させる名前のサフィックス。
const OBSOLETE_SUFFIX: &str = ".obsolete-";

// yt-dlp が使える状態でなければ取得し、実行権限を保証して返す。
pub fn ensure_yt_dlp() -> Result<PathBuf, String> {
    let yt_dlp = yt_dlp_path();
    if is_usable_tool(&yt_dlp) {
        ensure_executable(&yt_dlp)?;
        return Ok(yt_dlp);
    }

    if restore_interrupted_update(&yt_dlp) {
        crate::log_info!(Setup, "中断された更新からyt-dlpを復元しました。");
        ensure_executable(&yt_dlp)?;
        return Ok(yt_dlp);
    }

    if yt_dlp.exists() {
        crate::log_info!(Setup, "yt-dlpが壊れています。再ダウンロードします。");
    } else {
        crate::log_info!(Setup, "yt-dlpが見つかりません。ダウンロードします。");
    }
    install_yt_dlp()
}

// deno が使える状態でなければ取得し、実行権限を保証して返す。
pub fn ensure_deno() -> Result<PathBuf, String> {
    let deno = deno_path();
    if is_usable_tool(&deno) {
        ensure_executable(&deno)?;
        return Ok(deno);
    }

    if restore_interrupted_update(&deno) {
        crate::log_info!(Setup, "中断された更新からdenoを復元しました。");
        ensure_executable(&deno)?;
        return Ok(deno);
    }

    if deno.exists() {
        crate::log_info!(Setup, "denoが壊れています。再ダウンロードします。");
    } else {
        crate::log_info!(Setup, "denoが見つかりません。ダウンロードします。");
    }
    install_deno()
}

// Windowsはffmpeg/ffprobeを同梱しないため、未導入の場合のみ公式ビルドを取得する。
// 使用可能なものがbinフォルダかPATH上にあれば何もしない。
#[cfg(target_os = "windows")]
pub fn ensure_ffmpeg_tools() -> Result<(), String> {
    // ロックが毒されていても取得自体は再試行できるため、中身をそのまま使う。
    let _guard = FFMPEG_INSTALL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if crate::paths::media_tools_ready() {
        return Ok(());
    }

    crate::log_info!(
        Setup,
        "ffmpeg/ffprobeが見つかりません。ダウンロードします。"
    );
    install_ffmpeg_tools()?;

    if !crate::paths::media_tools_ready() {
        return Err("ffmpeg/ffprobeを配置しましたが、実行確認に失敗しました。".to_string());
    }
    crate::log_info!(Setup, "ffmpeg/ffprobeをダウンロードしました。");
    Ok(())
}

// 公式ビルドのZIPを一時フォルダへ取得し、ffmpegとffprobeをまとめて配置する。
#[cfg(target_os = "windows")]
fn install_ffmpeg_tools() -> Result<(), String> {
    let url = if cfg!(target_arch = "aarch64") {
        FFMPEG_WINDOWS_ARM64_URL
    } else {
        FFMPEG_WINDOWS_X64_URL
    };

    // PATH上のffmpegを書き換えないよう、配置先はアプリ用binフォルダに固定する。
    let work_dir = bin_dir();
    ensure_dir(&work_dir)?;
    cleanup_stale_staging_dirs(&work_dir);

    let staging = create_tool_staging_dir(&work_dir, "ffmpeg")?;
    let result = fetch_and_place_ffmpeg(url, &staging, &work_dir);
    let _ = fs::remove_dir_all(&staging);
    result
}

// ZIPの取得・展開・検証を終えてから、ffmpegとffprobeを本体へ入れ替える。
#[cfg(target_os = "windows")]
fn fetch_and_place_ffmpeg(url: &str, staging: &Path, install_dir: &Path) -> Result<(), String> {
    let zip_path = staging.join("ffmpeg.zip");
    curl_download(url, &zip_path, "ffmpeg")?;
    extract_tool_zip(&zip_path, staging)?;
    let _ = fs::remove_file(&zip_path);

    crate::log_info!(Setup, "ffmpeg/ffprobeのダウンロード内容を確認します。");

    let mut staged = Vec::new();
    for label in ["ffmpeg", "ffprobe"] {
        let file_name = executable_name(label);
        let found = find_extracted_file(staging, &file_name)
            .ok_or_else(|| format!("展開後の{file_name}が見つかりません。"))?;
        verify_staged_tool(&found, label)?;
        staged.push((found, install_dir.join(&file_name), label));
    }

    // 片方だけ入れ替わった状態にしないよう、両方の検証が終わってから配置する。
    for (found, target, label) in staged {
        replace_tool(&found, &target, label)?;
        remove_previous_versions(&target);
    }
    Ok(())
}

// 公式ZIPは`<展開名>/bin/`配下に実行ファイルを置くため、展開先を再帰的に探索する。
#[cfg(target_os = "windows")]
fn find_extracted_file(root: &Path, file_name: &str) -> Option<PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .flatten()
        .find(|entry| {
            entry.file_type().is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(file_name)
        })
        .map(|entry| entry.path().to_path_buf())
}

// 取得と検証が完了してから既存バイナリを置き換える。失敗時は旧バージョンを残す。
pub fn update_yt_dlp() -> Result<PathBuf, String> {
    crate::log_info!(Setup, "yt-dlpの最新版をダウンロードします。");
    install_yt_dlp()
}

// 取得と検証が完了してから既存バイナリを置き換える。失敗時は旧バージョンを残す。
pub fn update_deno() -> Result<PathBuf, String> {
    crate::log_info!(Setup, "denoの最新版をダウンロードします。");
    install_deno()
}

// yt-dlp を一時フォルダへ取得し、検証後に本体へ入れ替える。
fn install_yt_dlp() -> Result<PathBuf, String> {
    let installed = install_tool_staged(&yt_dlp_path(), "yt-dlp", |staging| {
        #[cfg(not(target_os = "windows"))]
        let url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos";
        #[cfg(target_os = "windows")]
        let url = if cfg!(target_arch = "aarch64") {
            "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_arm64.exe"
        } else {
            "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
        };
        let staged = staging.join(executable_name("yt-dlp"));
        curl_download(url, &staged, "yt-dlp")?;
        Ok(staged)
    })?;

    crate::log_info!(Setup, "yt-dlpをダウンロードしました。");
    Ok(installed)
}

// deno を一時フォルダへ取得・展開し、検証後に本体へ入れ替える。
fn install_deno() -> Result<PathBuf, String> {
    let installed = install_tool_staged(&deno_path(), "deno", |staging| {
        #[cfg(not(target_os = "windows"))]
        let url = "https://github.com/denoland/deno/releases/latest/download/deno-aarch64-apple-darwin.zip";
        #[cfg(target_os = "windows")]
        let url = if cfg!(target_arch = "aarch64") {
            "https://github.com/denoland/deno/releases/latest/download/deno-aarch64-pc-windows-msvc.zip"
        } else {
            "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip"
        };
        let zip_path = staging.join("deno.zip");
        curl_download(url, &zip_path, "deno")?;

        extract_tool_zip(&zip_path, staging)?;
        let _ = fs::remove_file(&zip_path);

        let staged = staging.join(executable_name("deno"));
        if !staged.exists() {
            return Err("展開後のdenoが見つかりません。".to_string());
        }
        Ok(staged)
    })?;

    crate::log_info!(Setup, "denoをダウンロードしました。");
    Ok(installed)
}

// 取得 → 検証 → 置き換え → 旧バージョン削除の順に処理し、途中失敗時は本体を触らない。
fn install_tool_staged<F>(target: &Path, label: &str, fetch: F) -> Result<PathBuf, String>
where
    F: FnOnce(&Path) -> Result<PathBuf, String>,
{
    // 一時フォルダは本体と同じフォルダに作り、置き換えを同一ボリューム内の rename で済ませる。
    let work_dir = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(bin_dir);
    ensure_dir(&work_dir)?;
    cleanup_stale_staging_dirs(&work_dir);

    let staging = create_tool_staging_dir(&work_dir, label)?;
    let result = fetch(&staging).and_then(|staged| {
        crate::log_info!(Setup, "{label}のダウンロード内容を確認します。");
        verify_staged_tool(&staged, label)?;
        replace_tool(&staged, target, label)
    });
    let _ = fs::remove_dir_all(&staging);

    result.map(|()| {
        // 新バージョンの配置後に、退避しておいた前バージョンを削除する。
        remove_previous_versions(target);
        target.to_path_buf()
    })
}

// 一時フォルダのバイナリを本体として採用し、旧バージョンを取り除く。
fn replace_tool(staged: &Path, target: &Path, label: &str) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        ensure_dir(parent)?;
    }

    // rename は既存ファイルを原子的に置き換えるため、不在状態が発生しない。
    match fs::rename(staged, target) {
        Ok(()) => return Ok(()),
        Err(err) if !target.exists() => {
            return Err(format!("{label}の配置に失敗しました: {err}"));
        }
        Err(_) => {}
    }

    // 実行中などで直接置き換えられない場合は、旧バージョンを退避してから配置する。
    let obsolete = next_obsolete_path(target);
    fs::rename(target, &obsolete)
        .map_err(|err| format!("{label}の旧バージョンの退避に失敗しました: {err}"))?;

    match fs::rename(staged, target) {
        Ok(()) => {
            let _ = fs::remove_file(&obsolete);
            Ok(())
        }
        Err(err) => {
            // 配置に失敗したら旧バージョンを戻し、ツールが消えた状態にしない。
            match fs::rename(&obsolete, target) {
                Ok(()) => Err(format!("{label}の配置に失敗しました: {err}")),
                Err(restore_err) => Err(format!(
                    "{label}の配置に失敗し、旧バージョンの復元にも失敗しました: {restore_err} (配置エラー: {err})"
                )),
            }
        }
    }
}

// ダウンロード結果がバイナリとして成立しているかを確認する。
fn verify_staged_tool(path: &Path, label: &str) -> Result<(), String> {
    let metadata =
        fs::metadata(path).map_err(|err| format!("{label}の確認に失敗しました: {err}"))?;
    if !metadata.is_file() {
        return Err(format!(
            "{label}のダウンロード結果がファイルではありません。"
        ));
    }
    if metadata.len() < MIN_TOOL_BYTES {
        return Err(format!(
            "{label}のダウンロードが不完全です（{}バイト）。",
            metadata.len()
        ));
    }

    ensure_executable(path)?;

    let output = hidden_command(path)
        .arg(tool_version_arg(label))
        .output()
        .map_err(|err| format!("{label}の起動確認に失敗しました: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{label}の起動確認に失敗しました: {}",
            output.status
        ));
    }
    Ok(())
}

// FFmpeg系は長いオプション形式の `--version` を受け付けない。
fn tool_version_arg(label: &str) -> &'static str {
    match label {
        "ffmpeg" | "ffprobe" => "-version",
        _ => "--version",
    }
}

// 本体が存在し、バイナリとして成立しているかを判定する。
fn is_usable_tool(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() >= MIN_TOOL_BYTES)
        .unwrap_or(false)
}

// ツールごとに衝突しない一時作業フォルダを作成する。
fn create_tool_staging_dir(work_dir: &Path, label: &str) -> Result<PathBuf, String> {
    let pid = std::process::id();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    for idx in 0..1000u32 {
        let candidate = work_dir.join(format!("{STAGING_PREFIX}{label}-{pid}-{timestamp}-{idx}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(format!("{label}の一時フォルダ作成に失敗しました: {err}")),
        }
    }
    Err(format!("{label}の一時フォルダ名の確保に失敗しました。"))
}

// 前回起動時に残った一時フォルダを掃除する。
fn cleanup_stale_staging_dirs(work_dir: &Path) {
    let pid_marker = format!("-{}-", std::process::id());
    let entries = match fs::read_dir(work_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(STAGING_PREFIX) {
            // 同一プロセスの進行中フォルダは消さない。
            continue;
        }
        if name.contains(&pid_marker) || !is_stale_staging_dir(&entry.path()) {
            continue;
        }
        let _ = fs::remove_dir_all(entry.path());
    }
}

fn is_stale_staging_dir(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= STALE_STAGING_AGE)
}

// 対象ツールの前バージョン（退避ファイルと旧仕様のバックアップ）を削除する。
fn remove_previous_versions(target: &Path) {
    for path in previous_version_paths(target) {
        let _ = fs::remove_file(path);
    }
}

// 旧仕様の更新中断で退避されたままのバイナリを本体へ戻す。戻せた場合のみ true。
fn restore_interrupted_update(target: &Path) -> bool {
    for path in previous_version_paths(target) {
        if !is_usable_tool(&path) {
            let _ = fs::remove_file(&path);
            continue;
        }
        if fs::rename(&path, target).is_ok() {
            return true;
        }
    }
    false
}

// 対象ツールの退避ファイル候補を列挙する。
fn previous_version_paths(target: &Path) -> Vec<PathBuf> {
    let (Some(parent), Some(file_name)) = (
        target.parent(),
        target.file_name().and_then(|name| name.to_str()),
    ) else {
        return Vec::new();
    };
    let prefixes = [
        format!("{file_name}{OBSOLETE_SUFFIX}"),
        format!("{file_name}.update-backup."),
    ];

    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            prefixes.iter().any(|prefix| name.starts_with(prefix))
        })
        .map(|entry| entry.path())
        .collect()
}

// 旧バージョンの退避先として未使用のパスを探す。
fn next_obsolete_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tool");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();

    for idx in 0..1000u32 {
        let candidate = parent.join(format!("{file_name}{OBSOLETE_SUFFIX}{pid}.{idx}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{file_name}{OBSOLETE_SUFFIX}fallback"))
}

// 実行可能な deno を探索し、yt-dlp に渡す runtime 指定文字列を返す。
pub fn js_runtime_arg() -> String {
    match detect_deno_binary() {
        Some(path) => format!("deno:{}", path.to_string_lossy()),
        None => "deno".to_string(),
    }
}

// ダウンロード仕様に関わらず共通で渡す引数セットを組み立てる。
fn common_yt_dlp_args(cookie_args: &[String]) -> Vec<String> {
    let mut args = vec![
        "--no-playlist".to_string(),
        "--encoding".to_string(),
        "utf-8".to_string(),
        "--newline".to_string(),
        "--progress-delta".to_string(),
        "1".to_string(),
    ];
    args.extend(cookie_args.iter().cloned());
    args.extend(vec![
        "--extractor-args".to_string(),
        "youtube:player_client=web".to_string(),
        "--extractor-args".to_string(),
        "youtube:skip=translated_subs".to_string(),
        "--concurrent-fragments".to_string(),
        "4".to_string(),
    ]);
    args
}

// ffmpeg と JS ランタイムの場所指定を末尾へ追加する。
fn append_runtime_args(args: &mut Vec<String>, ffmpeg_path: &str, js_runtime: &str) {
    args.push("--ffmpeg-location".to_string());
    args.push(ffmpeg_path.to_string());
    args.push("--js-runtimes".to_string());
    args.push(js_runtime.to_string());
}

// H.264 を優先して取得する引数。max_height を指定した場合はその解像度を上限にする。
fn h264_priority_args(max_height: Option<u32>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(height) = max_height {
        args.push("-f".to_string());
        args.push(format!("bv*[height<={height}]+ba/b[height<={height}]"));
    }
    args.push("-S".to_string());
    args.push("vcodec:h264,res,acodec:m4a".to_string());
    args.push("--match-filter".to_string());
    args.push("vcodec~='(?i)^(avc|h264)'".to_string());
    args.push("--merge-output-format".to_string());
    args.push("mp4".to_string());
    args
}

// yt-dlp の通常ダウンロード用引数セットを、選択中のダウンロード仕様に合わせて組み立てる。
pub(super) fn base_yt_dlp_args(
    mode: DownloadMode,
    ffmpeg_path: &str,
    cookie_args: &[String],
    js_runtime: &str,
) -> Vec<String> {
    let mut args = common_yt_dlp_args(cookie_args);
    match mode {
        DownloadMode::Standard => args.extend(h264_priority_args(None)),
        DownloadMode::UpTo1080p => args.extend(h264_priority_args(Some(1080))),
        DownloadMode::BestThenConvert => {
            // コーデックを制限せず最高画質を取得する。変換はダウンロード後に自前の ffmpeg で行うため、
            // どのコーデックでも失敗しない mkv へ結合しておく。
            args.push("-f".to_string());
            args.push("bv*+ba/b".to_string());
            args.push("--merge-output-format".to_string());
            args.push("mkv".to_string());
        }
    }
    append_runtime_args(&mut args, ffmpeg_path, js_runtime);
    args
}

// H.264 優先モードが失敗した場合のフォールバック引数セットを組み立てる。
// 最高画質モードはコーデックを制限しないため、再試行する余地がなく None を返す。
pub(super) fn fallback_yt_dlp_args(
    mode: DownloadMode,
    ffmpeg_path: &str,
    cookie_args: &[String],
    js_runtime: &str,
) -> Option<Vec<String>> {
    let max_height = match mode {
        DownloadMode::Standard => 720,
        DownloadMode::UpTo1080p => 1080,
        DownloadMode::BestThenConvert => return None,
    };

    let mut args = common_yt_dlp_args(cookie_args);
    args.push("-f".to_string());
    args.push(format!(
        "bv*[height<={max_height}]+ba/b[height<={max_height}]"
    ));
    args.push("--recode-video".to_string());
    args.push("mp4".to_string());
    args.push("--postprocessor-args".to_string());
    args.push(format!(
        "VideoConvertor:-c:v {} -b:v 5M -pix_fmt yuv420p",
        h264_encoder()
    ));
    append_runtime_args(&mut args, ffmpeg_path, js_runtime);
    Some(args)
}

fn detect_deno_binary() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("DENO_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("DENO_BIN") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(deno_path());

    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".deno").join("bin").join(executable_name("deno")));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/deno"));
    candidates.push(PathBuf::from("/usr/local/bin/deno"));

    if let Some(path_env) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_env) {
            candidates.push(dir.join(executable_name("deno")));
        }
    }

    candidates
        .into_iter()
        .find(|candidate| candidate.exists() && is_executable(candidate))
}

fn ensure_executable(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|err| err.to_string())?;
    if !metadata.is_file() {
        return Err(format!("実行ファイルではありません: {}", path.display()));
    }
    #[cfg(unix)]
    {
        let mut perms = metadata.permissions();
        let mode = perms.mode();
        if mode & 0o111 != 0o111 {
            perms.set_mode(mode | 0o111);
            fs::set_permissions(path, perms).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn curl_download(url: &str, output_path: &Path, label: &str) -> Result<(), String> {
    let status = hidden_command("curl")
        .arg("-L")
        // HTTPエラー応答を成果物として保存しないよう失敗扱いにする。
        .arg("--fail")
        .arg("--retry")
        .arg("3")
        .arg("--retry-delay")
        .arg("2")
        .arg("--retry-connrefused")
        .arg("--connect-timeout")
        .arg("30")
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

fn extract_tool_zip(zip_path: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let output = hidden_command("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
            "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.IO.Compression.FileSystem; [System.IO.Compression.ZipFile]::ExtractToDirectory($env:VJDL_ZIP_PATH, $env:VJDL_ZIP_DEST)"])
        .env("VJDL_ZIP_PATH", zip_path)
        .env("VJDL_ZIP_DEST", destination)
        .output();
    #[cfg(not(target_os = "windows"))]
    let output = hidden_command("unzip")
        .arg("-o")
        .arg(zip_path)
        .arg("-d")
        .arg(destination)
        .output();
    let output = output.map_err(|err| format!("ZIP展開処理の起動に失敗しました: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "ZIPの展開に失敗しました: {} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::Path;

    use super::{
        MIN_TOOL_BYTES, common_yt_dlp_args, hidden_command, install_tool_staged, is_usable_tool,
        previous_version_paths, replace_tool, restore_interrupted_update, tool_version_arg,
        verify_staged_tool,
    };

    // --version に応答し、サイズ下限も満たすダミーバイナリを作る。
    fn write_dummy_tool(path: &Path, marker: &str) {
        let padding = "#".repeat(MIN_TOOL_BYTES as usize);
        let script = format!("#!/bin/sh\necho \"{marker}\"\n{padding}\n");
        let mut file = fs::File::create(path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        super::ensure_executable(path).unwrap();
    }

    #[test]
    fn replace_tool_keeps_target_present_and_removes_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("yt-dlp");
        let staged = dir.path().join("staged");
        write_dummy_tool(&target, "old");
        write_dummy_tool(&staged, "new");

        replace_tool(&staged, &target, "yt-dlp").unwrap();

        let output = hidden_command(&target).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "new");
        assert!(previous_version_paths(&target).is_empty());
    }

    #[test]
    fn verify_staged_tool_rejects_truncated_download() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("yt-dlp");
        fs::write(&staged, b"<html>404</html>").unwrap();

        assert!(verify_staged_tool(&staged, "yt-dlp").is_err());
    }

    #[test]
    fn uses_ffmpeg_compatible_version_argument() {
        assert_eq!(tool_version_arg("ffmpeg"), "-version");
        assert_eq!(tool_version_arg("ffprobe"), "-version");
        assert_eq!(tool_version_arg("yt-dlp"), "--version");
        assert_eq!(tool_version_arg("deno"), "--version");
    }

    #[test]
    fn failed_install_leaves_previous_version_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("yt-dlp");
        write_dummy_tool(&target, "old");

        let result = install_tool_staged(&target, "yt-dlp", |staging| {
            // 遅い回線でダウンロードが途中で切れた状況を再現する。
            let staged = staging.join("yt-dlp");
            fs::write(&staged, b"partial").unwrap();
            Ok(staged)
        });

        assert!(result.is_err());
        assert!(is_usable_tool(&target));
        let output = hidden_command(&target).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "old");
    }

    #[test]
    fn restore_interrupted_update_recovers_legacy_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("yt-dlp");
        let backup = dir.path().join("yt-dlp.update-backup.1234");
        write_dummy_tool(&backup, "old");

        assert!(restore_interrupted_update(&target));
        assert!(is_usable_tool(&target));
        assert!(!backup.exists());
    }

    #[test]
    fn restore_interrupted_update_ignores_broken_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("yt-dlp");
        let backup = dir.path().join("yt-dlp.update-backup.1234");
        fs::write(&backup, b"partial").unwrap();

        assert!(!restore_interrupted_update(&target));
        assert!(!target.exists());
        assert!(!backup.exists());
    }

    #[test]
    fn yt_dlp_output_is_utf8_and_progress_is_throttled() {
        let args = common_yt_dlp_args(&[]);

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--encoding" && pair[1] == "utf-8")
        );
        assert!(args.iter().any(|arg| arg == "--newline"));
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--progress-delta" && pair[1] == "1")
        );
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_setup_tests {
    use super::*;

    #[test]
    fn extracts_zip_in_path_with_spaces_and_metacharacters() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("setup space [test] ' 日本語");
        fs::create_dir(&dir).unwrap();
        let source = dir.join("deno.exe");
        fs::write(&source, b"zip extraction fixture").unwrap();
        let zip = temp.path().join("test.zip");
        let status = hidden_command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.IO.Compression.FileSystem; [System.IO.Compression.ZipFile]::CreateFromDirectory($env:VJDL_TEST_SOURCE, $env:VJDL_TEST_ZIP)"])
            .env("VJDL_TEST_SOURCE", &dir).env("VJDL_TEST_ZIP", &zip)
            .status().unwrap();
        assert!(status.success());
        let dest = dir.join("output");
        extract_tool_zip(&zip, &dest).unwrap();
        assert_eq!(
            fs::read(dest.join("deno.exe")).unwrap(),
            b"zip extraction fixture"
        );
        assert!(yt_dlp_path().ends_with("yt-dlp.exe"));
        assert!(deno_path().ends_with("deno.exe"));
    }
}
