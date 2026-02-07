use arboard::Clipboard;
use std::fs;
use std::io::{BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;

use crate::fs_utils::ensure_dir;
use crate::paths::{bin_dir, deno_path, ffmpeg_path, yt_dlp_path};

pub enum DownloadEvent {
    Log(String),
    File,
    Progress(ProgressUpdate),
    Done(Result<(), String>),
}

#[derive(Clone, Debug)]
pub struct ProgressUpdate {
    pub message: String,
    pub progress: f32,
    pub visible: bool,
}

impl ProgressUpdate {
    pub fn info_loading(elapsed: &str) -> Self {
        Self {
            message: format!("動画読み込み中...{}", format_elapsed(elapsed)),
            progress: -1.0,
            visible: true,
        }
    }

    pub fn downloading(percent: f32, elapsed: &str) -> Self {
        let clamped = percent.clamp(0.0, 100.0);
        Self {
            message: format!(
                "ダウンロード中... {:.1}%{}",
                clamped,
                format_elapsed(elapsed)
            ),
            progress: clamped / 100.0,
            visible: true,
        }
    }

    pub fn post_processing(elapsed: &str) -> Self {
        Self {
            message: format!("変換中...{}", format_elapsed(elapsed)),
            progress: -1.0,
            visible: true,
        }
    }

    pub fn completed(elapsed: &str) -> Self {
        Self {
            message: format!("ダウンロード完了!{}", format_elapsed(elapsed)),
            progress: 1.0,
            visible: true,
        }
    }

    pub fn hidden() -> Self {
        Self {
            message: String::new(),
            progress: 0.0,
            visible: false,
        }
    }
}

struct ProgressContext {
    start: Instant,
    active: Arc<AtomicBool>,
    progress_started: AtomicBool,
    post_processing: AtomicBool,
}

impl ProgressContext {
    fn new(active: Arc<AtomicBool>) -> Arc<Self> {
        active.store(true, Ordering::Relaxed);
        Arc::new(Self {
            start: Instant::now(),
            active,
            progress_started: AtomicBool::new(false),
            post_processing: AtomicBool::new(false),
        })
    }

    fn elapsed(&self) -> String {
        let elapsed = self.start.elapsed().as_secs();
        let hours = elapsed / 3600;
        let minutes = (elapsed % 3600) / 60;
        let seconds = elapsed % 60;
        if hours > 0 {
            format!("{hours}:{minutes:02}:{seconds:02}")
        } else {
            format!("{minutes:02}:{seconds:02}")
        }
    }

    fn mark_progress_started(&self) {
        self.progress_started.store(true, Ordering::Relaxed);
    }

    fn progress_started(&self) -> bool {
        self.progress_started.load(Ordering::Relaxed)
    }

    fn set_post_processing(&self) {
        self.post_processing.store(true, Ordering::Relaxed);
    }

    fn post_processing(&self) -> bool {
        self.post_processing.load(Ordering::Relaxed)
    }

    fn deactivate(&self) {
        self.active.store(false, Ordering::Relaxed);
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
}

pub fn run_download(
    url: String,
    output_dir: PathBuf,
    cookie_args: Vec<String>,
    tx: mpsc::Sender<DownloadEvent>,
    active_flag: Arc<AtomicBool>,
) {
    let progress = ProgressContext::new(active_flag);
    let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::info_loading(
        &progress.elapsed(),
    )));
    start_loading_elapsed_ticker(progress.clone(), tx.clone());

    let result = run_download_inner(url, output_dir, cookie_args, &tx, &progress);

    finalize_progress(&progress, &tx, result.is_ok());
    let _ = tx.send(DownloadEvent::Done(result));
}

fn run_download_inner(
    url: String,
    output_dir: PathBuf,
    cookie_args: Vec<String>,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
) -> Result<(), String> {
    let yt_dlp_path = ensure_yt_dlp(tx)?;

    if let Err(err) = ensure_dir(&output_dir) {
        return Err(format!("保存先フォルダの作成に失敗しました: {err}"));
    }

    let output_template = output_dir.join("%(title)s.%(ext)s");
    let ffmpeg_path = ffmpeg_path();
    let ffmpeg_arg = if ffmpeg_path.exists() {
        Some(ffmpeg_path.to_string_lossy().to_string())
    } else {
        let _ = tx.send(DownloadEvent::Log(
            "ffmpegが見つからないため、変換/結合は一部スキップされます。".to_string(),
        ));
        None
    };
    let use_deno = deno_path().exists();

    let mut args = Vec::new();
    args.extend(base_yt_dlp_args(
        ffmpeg_arg.as_deref(),
        use_deno,
        &cookie_args,
    ));
    args.push("-o".to_string());
    args.push(output_template.to_string_lossy().to_string());
    args.push(url.clone());

    let status = run_yt_dlp(&yt_dlp_path, &args, tx, progress.clone());
    match status {
        Ok(code) if code.success() => return Ok(()),
        Ok(_) => {
            let _ = tx.send(DownloadEvent::Log(
                "H.264優先モードに失敗。互換モードで再試行します。".to_string(),
            ));
        }
        Err(err) => return Err(format!("yt-dlpの実行に失敗しました: {err}")),
    }

    let mut fallback_args = Vec::new();
    fallback_args.extend(fallback_yt_dlp_args(
        ffmpeg_arg.as_deref(),
        use_deno,
        &cookie_args,
    ));
    fallback_args.push("-o".to_string());
    fallback_args.push(output_template.to_string_lossy().to_string());
    fallback_args.push(url);

    let status = run_yt_dlp(&yt_dlp_path, &fallback_args, tx, progress.clone());
    match status {
        Ok(code) if code.success() => Ok(()),
        Ok(code) => Err(format!("yt-dlp exited with status: {code}")),
        Err(err) => Err(format!("yt-dlpの実行に失敗しました: {err}")),
    }
}

pub fn read_clipboard_url() -> Result<String, String> {
    let mut clipboard = Clipboard::new()
        .map_err(|err| format!("クリップボードにアクセスできません: {err}"))?;
    let text = clipboard
        .get_text()
        .map_err(|err| format!("クリップボードの取得に失敗しました: {err}"))?;
    extract_url_from_text(&text)
        .ok_or_else(|| "クリップボードにURLがありません。".to_string())
}

fn ensure_yt_dlp(tx: &mpsc::Sender<DownloadEvent>) -> Result<PathBuf, String> {
    let yt_dlp = yt_dlp_path();
    if yt_dlp.exists() {
        ensure_executable(&yt_dlp)?;
        return Ok(yt_dlp);
    }

    let bin = bin_dir();
    ensure_dir(&bin)?;
    let _ = tx.send(DownloadEvent::Log(
        "yt-dlpが見つかりません。ダウンロードします。".to_string(),
    ));

    let url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos";
    let status = Command::new("curl")
        .arg("-L")
        .arg("-o")
        .arg(yt_dlp.to_string_lossy().to_string())
        .arg(url)
        .status()
        .map_err(|err| format!("curl起動に失敗しました: {err}"))?;

    if !status.success() {
        return Err(format!("yt-dlpのダウンロードに失敗しました: {status}"));
    }

    ensure_executable(&yt_dlp)?;
    let _ = tx.send(DownloadEvent::Log(
        "yt-dlpをダウンロードしました。".to_string(),
    ));
    Ok(yt_dlp)
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

fn base_yt_dlp_args(
    ffmpeg_path: Option<&str>,
    use_deno: bool,
    cookie_args: &[String],
) -> Vec<String> {
    let mut args = vec![
        "--print".to_string(),
        "after_move:filepath".to_string(),
        "--no-playlist".to_string(),
    ];
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

    if let Some(ffmpeg_path) = ffmpeg_path {
        args.push("--merge-output-format".to_string());
        args.push("mp4".to_string());
        args.push("--ffmpeg-location".to_string());
        args.push(ffmpeg_path.to_string());
    }

    if use_deno {
        args.push("--js-runtimes".to_string());
        args.push("deno".to_string());
    }

    args
}

fn fallback_yt_dlp_args(
    ffmpeg_path: Option<&str>,
    use_deno: bool,
    cookie_args: &[String],
) -> Vec<String> {
    let mut args = vec![
        "--print".to_string(),
        "after_move:filepath".to_string(),
        "--no-playlist".to_string(),
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

    if let Some(ffmpeg_path) = ffmpeg_path {
        args.push("-f".to_string());
        args.push("bv*[height<=720]+ba/b[height<=720]".to_string());
        args.push("--recode-video".to_string());
        args.push("mp4".to_string());
        args.push("--postprocessor-args".to_string());
        args.push(
            "VideoConvertor:-c:v h264_videotoolbox -b:v 5M -pix_fmt yuv420p".to_string(),
        );
        args.push("--ffmpeg-location".to_string());
        args.push(ffmpeg_path.to_string());
    } else {
        args.push("-f".to_string());
        args.push("b[height<=720]".to_string());
    }

    if use_deno {
        args.push("--js-runtimes".to_string());
        args.push("deno".to_string());
    }

    args
}

fn run_yt_dlp(
    yt_dlp_path: &Path,
    args: &[String],
    tx: &mpsc::Sender<DownloadEvent>,
    progress: Arc<ProgressContext>,
) -> Result<std::process::ExitStatus, String> {
    let mut command = Command::new(yt_dlp_path);
    command.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    let bin = bin_dir();
    if bin.exists() {
        let mut paths = Vec::new();
        paths.push(bin.as_os_str().to_owned());
        if let Some(current) = std::env::var_os("PATH") {
            paths.push(current);
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            command.env("PATH", joined);
        }
    }

    let mut child = command
        .spawn()
        .map_err(|err| format!("yt-dlpの起動に失敗しました: {err}"))?;

    if let Some(stdout) = child.stdout.take() {
        let tx_stdout = tx.clone();
        let progress_ctx = progress.clone();
        thread::spawn(move || stream_lines(stdout, tx_stdout, progress_ctx));
    }

    if let Some(stderr) = child.stderr.take() {
        let tx_stderr = tx.clone();
        let progress_ctx = progress.clone();
        thread::spawn(move || stream_lines(stderr, tx_stderr, progress_ctx));
    }

    child.wait().map_err(|err| err.to_string())
}

fn stream_lines<R: Read + Send + 'static>(
    reader: R,
    tx: mpsc::Sender<DownloadEvent>,
    progress: Arc<ProgressContext>,
) {
    let mut buffered = BufReader::new(reader);
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    loop {
        let read = match buffered.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        for &byte in &buf[..read] {
            if byte == b'\n' || byte == b'\r' {
                if !line.is_empty() {
                    if let Ok(text) = String::from_utf8(line.clone()) {
                        handle_stream_line(text, &tx, &progress);
                    } else {
                        let text = String::from_utf8_lossy(&line).to_string();
                        handle_stream_line(text, &tx, &progress);
                    }
                    line.clear();
                }
            } else {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        let text = String::from_utf8_lossy(&line).to_string();
        handle_stream_line(text, &tx, &progress);
    }
}

fn handle_stream_line(line: String, tx: &mpsc::Sender<DownloadEvent>, progress: &Arc<ProgressContext>) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    handle_progress_line(trimmed, progress, tx);

    if let Some(path) = maybe_path_from_line(trimmed) {
        let _ = tx.send(DownloadEvent::File);
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("downloaded file");
        let _ = tx.send(DownloadEvent::Log(format!("Saved: {label}")));
    } else {
        let _ = tx.send(DownloadEvent::Log(trimmed.to_string()));
    }
}

fn maybe_path_from_line(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim();
    let trimmed = trimmed.trim_matches('"');

    if let Some(rest) = trimmed.strip_prefix("Destination: ") {
        return Some(PathBuf::from(rest.trim_matches('"')));
    }

    if let Some(rest) = trimmed.strip_prefix("[download] Destination: ") {
        return Some(PathBuf::from(rest.trim_matches('"')));
    }

    if let Some(path) = extract_quoted_path(trimmed) {
        return Some(path);
    }

    if looks_like_path(trimmed) {
        return Some(PathBuf::from(trimmed));
    }

    None
}

fn looks_like_path(text: &str) -> bool {
    if text.starts_with('/') {
        return true;
    }
    let bytes = text.as_bytes();
    bytes.len() > 2 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn extract_quoted_path(text: &str) -> Option<PathBuf> {
    let start = text.find('"')? + 1;
    let end = text[start..].find('"')? + start;
    let candidate = &text[start..end];
    if looks_like_path(candidate) {
        Some(PathBuf::from(candidate))
    } else {
        None
    }
}

fn extract_url_from_text(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(url) = line.strip_prefix("URL=") {
            if is_valid_url(url) {
                return Some(url.trim().to_string());
            }
        }
    }

    if let Some(url) = extract_webloc_url(text) {
        return Some(url);
    }

    for token in text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        if token.starts_with("http://") || token.starts_with("https://") {
            if is_valid_url(token) {
                return Some(token.to_string());
            }
        }
    }

    None
}

fn extract_webloc_url(text: &str) -> Option<String> {
    let key = "<key>URL</key>";
    let string_open = "<string>";
    let string_close = "</string>";

    let key_pos = text.find(key)?;
    let after_key = &text[key_pos + key.len()..];
    let start = after_key.find(string_open)? + string_open.len();
    let after_start = &after_key[start..];
    let end = after_start.find(string_close)?;
    let url = after_start[..end].trim();
    if is_valid_url(url) {
        Some(url.to_string())
    } else {
        None
    }
}

fn is_valid_url(candidate: &str) -> bool {
    Url::parse(candidate)
        .map(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or(false)
}

fn handle_progress_line(
    line: &str,
    progress: &Arc<ProgressContext>,
    tx: &mpsc::Sender<DownloadEvent>,
) {
    if progress.post_processing() {
        return;
    }

    if is_post_processing_line(line) {
        progress.mark_progress_started();
        progress.set_post_processing();
        let update = ProgressUpdate::post_processing(&progress.elapsed());
        let _ = tx.send(DownloadEvent::Progress(update));
        return;
    }

    if let Some(percent) = extract_percent(line) {
        progress.mark_progress_started();
        let update = ProgressUpdate::downloading(percent, &progress.elapsed());
        let _ = tx.send(DownloadEvent::Progress(update));
    }
}

fn extract_percent(line: &str) -> Option<f32> {
    let chars = line.chars().collect::<Vec<_>>();
    let mut idx = 0usize;
    while idx < chars.len() {
        if chars[idx] == '%' {
            let mut start = idx;
            while start > 0 {
                let c = chars[start - 1];
                if c.is_ascii_digit() || c == '.' {
                    start -= 1;
                } else {
                    break;
                }
            }
            if start < idx {
                let candidate: String = chars[start..idx].iter().collect();
                if let Ok(value) = candidate.parse::<f32>() {
                    return Some(value);
                }
            }
        }
        idx += 1;
    }
    None
}

fn is_post_processing_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("[merger]")
        || lower.contains("[ffmpeg]")
        || lower.contains("[extractaudio]")
        || lower.contains("[postprocess]")
        || lower.contains("[videoconvertor]")
        || lower.contains("[videoconverter]")
        || lower.contains("[audioconvertor]")
        || lower.contains("[audioconverter]")
        || lower.contains("[fixup")
        || lower.contains("merging formats into")
        || lower.contains("post-process")
}

fn format_elapsed(elapsed: &str) -> String {
    if elapsed.trim().is_empty() {
        String::new()
    } else {
        format!(" (経過: {elapsed})")
    }
}

fn start_loading_elapsed_ticker(progress: Arc<ProgressContext>, tx: mpsc::Sender<DownloadEvent>) {
    thread::spawn(move || {
        while progress.is_active() && !progress.progress_started() {
            let update = ProgressUpdate::info_loading(&progress.elapsed());
            let _ = tx.send(DownloadEvent::Progress(update));
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn finalize_progress(
    progress: &Arc<ProgressContext>,
    tx: &mpsc::Sender<DownloadEvent>,
    success: bool,
) {
    let elapsed = progress.elapsed();
    progress.deactivate();
    if success {
        let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::completed(&elapsed)));
        schedule_progress_hide_if_idle(progress.active.clone(), tx.clone());
    } else {
        let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::hidden()));
    }
}

fn schedule_progress_hide_if_idle(active: Arc<AtomicBool>, tx: mpsc::Sender<DownloadEvent>) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1200));
        if !active.load(Ordering::Relaxed) {
            let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::hidden()));
        }
    });
}
