use arboard::Clipboard;
use std::fs;
use std::io::{BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

use crate::bundled::ensure_bundled_tools;
use crate::fs_utils::{ensure_dir, is_executable};
use crate::paths::{bin_dir, deno_path, ffmpeg_path, yt_dlp_path};

pub enum DownloadEvent {
    Log(String),
    Progress(ProgressUpdate),
    Done(Result<(), String>),
}

pub const CANCELLED_ERROR: &str = "__CANCELLED__";
const ANIMETHEMES_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

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

#[derive(Clone, Default)]
pub struct ProcessTracker {
    pids: Arc<Mutex<Vec<u32>>>,
}

impl ProcessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, child: &Child) {
        let pid = child.id();
        if pid == 0 {
            return;
        }
        let mut pids = self.pids.lock().unwrap();
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }

    pub fn terminate_all(&self) {
        let pids = {
            let pids = self.pids.lock().unwrap();
            pids.clone()
        };
        for pid in &pids {
            let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
        }
        for pid in &pids {
            let _ = Command::new("kill").arg("-KILL").arg(pid.to_string()).status();
        }
    }
}

pub fn run_download(
    url: String,
    output_dir: PathBuf,
    cookie_args: Vec<String>,
    tx: mpsc::Sender<DownloadEvent>,
    active_flag: Arc<AtomicBool>,
    cancel_flag: Arc<AtomicBool>,
    tracker: ProcessTracker,
) {
    let progress = ProgressContext::new(active_flag);
    let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::info_loading(
        &progress.elapsed(),
    )));
    start_loading_elapsed_ticker(progress.clone(), tx.clone());

    let result = run_download_inner(
        url,
        output_dir,
        cookie_args,
        &tx,
        &progress,
        &cancel_flag,
        &tracker,
    );

    finalize_progress(&progress, &tx, result.is_ok());
    let _ = tx.send(DownloadEvent::Done(result));
}

fn run_download_inner(
    url: String,
    output_dir: PathBuf,
    cookie_args: Vec<String>,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    cancel_flag: &Arc<AtomicBool>,
    tracker: &ProcessTracker,
) -> Result<(), String> {
    if cancel_flag.load(Ordering::Relaxed) {
        return Err(CANCELLED_ERROR.to_string());
    }
    ensure_bundled_tools()?;
    let ffmpeg = ffmpeg_path();
    if !ffmpeg.exists() {
        return Err("ffmpegが見つかりません。".to_string());
    }

    let yt_dlp_path = yt_dlp_path();
    if !yt_dlp_path.exists() || !is_executable(&yt_dlp_path) {
        return Err("yt-dlpが見つかりません。".to_string());
    }

    if let Err(err) = ensure_dir(&output_dir) {
        return Err(format!("保存先フォルダの作成に失敗しました: {err}"));
    }

    if is_animethemes_url(&url) {
        return run_animethemes_pipeline(
            &url,
            &output_dir,
            &yt_dlp_path,
            &ffmpeg,
            tx,
            progress,
            cancel_flag,
            tracker,
        );
    }

    let output_template = output_dir.join("%(title)s.%(ext)s");
    let ffmpeg_arg = ffmpeg.to_string_lossy().to_string();

    let mut args = Vec::new();
    args.extend(base_yt_dlp_args(&ffmpeg_arg, &cookie_args));
    args.push("-o".to_string());
    args.push(output_template.to_string_lossy().to_string());
    args.push(url.clone());

    let status = run_yt_dlp(
        &yt_dlp_path,
        &args,
        tx,
        progress.clone(),
        true,
        tracker,
    );
    match status {
        Ok(code) if code.success() => return Ok(()),
        Ok(_) => {
            let _ = tx.send(DownloadEvent::Log(
                "H.264優先モードに失敗。互換モードで再試行します。".to_string(),
            ));
        }
        Err(err) => return Err(format!("yt-dlpの実行に失敗しました: {err}")),
    }
    if cancel_flag.load(Ordering::Relaxed) {
        return Err(CANCELLED_ERROR.to_string());
    }

    let mut fallback_args = Vec::new();
    fallback_args.extend(fallback_yt_dlp_args(&ffmpeg_arg, &cookie_args));
    fallback_args.push("-o".to_string());
    fallback_args.push(output_template.to_string_lossy().to_string());
    fallback_args.push(url);

    let status = run_yt_dlp(
        &yt_dlp_path,
        &fallback_args,
        tx,
        progress.clone(),
        true,
        tracker,
    );
    if cancel_flag.load(Ordering::Relaxed) {
        return Err(CANCELLED_ERROR.to_string());
    }
    match status {
        Ok(code) if code.success() => Ok(()),
        Ok(code) => Err(format!("yt-dlp exited with status: {code}")),
        Err(err) => Err(format!("yt-dlpの実行に失敗しました: {err}")),
    }
}

pub fn read_clipboard_text() -> Option<String> {
    let mut clipboard = Clipboard::new().ok()?;
    let text = clipboard.get_text().ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

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
    if let Some(tx) = tx {
        let _ = tx.send(DownloadEvent::Log(
            "yt-dlpをダウンロードしました。".to_string(),
        ));
    }
    Ok(yt_dlp)
}

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
    let url = "https://github.com/denoland/deno/releases/latest/download/deno-aarch64-apple-darwin.zip";
    let status = Command::new("curl")
        .arg("-L")
        .arg("-o")
        .arg(zip_path.to_string_lossy().to_string())
        .arg(url)
        .status()
        .map_err(|err| format!("curl起動に失敗しました: {err}"))?;

    if !status.success() {
        return Err(format!("denoのダウンロードに失敗しました: {status}"));
    }

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
        let _ = tx.send(DownloadEvent::Log("denoをダウンロードしました。".to_string()));
    }
    Ok(deno)
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

fn base_yt_dlp_args(ffmpeg_path: &str, cookie_args: &[String]) -> Vec<String> {
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

fn fallback_yt_dlp_args(ffmpeg_path: &str, cookie_args: &[String]) -> Vec<String> {
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
    args.push(
        "VideoConvertor:-c:v h264_videotoolbox -b:v 5M -pix_fmt yuv420p".to_string(),
    );
    args.push("--ffmpeg-location".to_string());
    args.push(ffmpeg_path.to_string());
    args.push("--js-runtimes".to_string());
    args.push("deno".to_string());

    args
}

fn is_animethemes_url(url: &str) -> bool {
    url.to_lowercase().contains("animethemes.moe")
}

fn run_animethemes_pipeline(
    url: &str,
    output_dir: &Path,
    yt_dlp: &Path,
    ffmpeg: &Path,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    cancel_flag: &Arc<AtomicBool>,
    tracker: &ProcessTracker,
) -> Result<(), String> {
    if cancel_flag.load(Ordering::Relaxed) {
        return Err(CANCELLED_ERROR.to_string());
    }
    let output_path = build_animethemes_output_path(url, output_dir);

    let direct_url = fetch_animethemes_direct_webm(url, tx)?;
    match direct_url {
        Some(webm_url) => {
            let _ = tx.send(DownloadEvent::Log(format!(
                "AnimeThemes直リンクを取得しました: {webm_url}"
            )));
            let mut cmd = Command::new("curl");
            cmd.arg("-L")
                .arg("-m")
                .arg("120")
                .arg("--fail")
                .arg("-o")
                .arg("-")
                .arg("-A")
                .arg(ANIMETHEMES_USER_AGENT)
                .arg(webm_url);
            if let Err(err) =
                run_pipe_to_ffmpeg(cmd, ffmpeg, &output_path, tx, progress, "webm", tracker)
            {
                if cancel_flag.load(Ordering::Relaxed) {
                    return Err(CANCELLED_ERROR.to_string());
                }
                return Err(err);
            }
        }
        None => {
            let _ = tx.send(DownloadEvent::Log(
                "AnimeThemes直リンク取得に失敗。yt-dlpでフォールバックします。".to_string(),
            ));
            let mut cmd = Command::new(yt_dlp);
            cmd.arg("--no-playlist")
                .arg("--concurrent-fragments")
                .arg("4")
                .arg("-f")
                .arg("bv+ba/b")
                .arg("--ffmpeg-location")
                .arg(ffmpeg.to_string_lossy().to_string())
                .arg("-o")
                .arg("-")
                .arg(url);
            if let Err(err) =
                run_pipe_to_ffmpeg(cmd, ffmpeg, &output_path, tx, progress, "webm", tracker)
            {
                if cancel_flag.load(Ordering::Relaxed) {
                    return Err(CANCELLED_ERROR.to_string());
                }
                return Err(err);
            }
        }
    }

    Ok(())
}

fn fetch_animethemes_direct_webm(
    url: &str,
    tx: &mpsc::Sender<DownloadEvent>,
) -> Result<Option<String>, String> {
    let mut cmd = Command::new("curl");
    cmd.arg("-sL")
        .arg("-m")
        .arg("8")
        .arg("-A")
        .arg(ANIMETHEMES_USER_AGENT)
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|err| format!("curl起動に失敗しました: {err}"))?;

    let mut buf = Vec::with_capacity(30_000);
    if let Some(mut stdout) = child.stdout.take() {
        let mut chunk = [0u8; 4096];
        while buf.len() < 30_000 {
            let remaining = 30_000 - buf.len();
            let read_size = remaining.min(chunk.len());
            let read = stdout
                .read(&mut chunk[..read_size])
                .map_err(|err| format!("curl出力の読み取りに失敗しました: {err}"))?;
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
        }
    }

    if child.try_wait().map_err(|err| err.to_string())?.is_none() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|err| err.to_string())?;
    if !status.success() {
        let _ = tx.send(DownloadEvent::Log(format!(
            "AnimeThemesページ取得に失敗しました: {}",
            status
        )));
        return Ok(None);
    }

    let html = String::from_utf8_lossy(&buf);
    Ok(extract_animethemes_webm(&html))
}

fn extract_animethemes_webm(html: &str) -> Option<String> {
    let og_prefix = "name=\"og:video\" content=\"";
    let video_prefix = "video src=\"";
    let og_pos = html.find(og_prefix);
    let video_pos = html.find(video_prefix);

    let (pos, prefix) = match (og_pos, video_pos) {
        (Some(og), Some(video)) => {
            if og <= video {
                (og, og_prefix)
            } else {
                (video, video_prefix)
            }
        }
        (Some(og), None) => (og, og_prefix),
        (None, Some(video)) => (video, video_prefix),
        (None, None) => return None,
    };

    let after = &html[pos + prefix.len()..];
    let end = after.find('"')?;
    let url = &after[..end];
    if url.starts_with("https://") && url.ends_with(".webm") {
        Some(url.to_string())
    } else {
        None
    }
}

fn build_animethemes_output_path(url: &str, output_dir: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let parsed = match Url::parse(url) {
        Ok(parsed) => parsed,
        Err(_) => {
            return output_dir.join(format!("animethemes-{timestamp}.mp4"));
        }
    };

    let mut segments = Vec::new();
    if let Some(items) = parsed.path_segments() {
        for item in items {
            let trimmed = item.trim();
            if !trimmed.is_empty() {
                segments.push(trimmed.to_string());
            }
        }
    }

    if segments.is_empty() {
        return output_dir.join(format!("animethemes-{timestamp}.mp4"));
    }

    let mut picked: Vec<String> = Vec::new();
    for idx in (0..segments.len()).rev() {
        let seg = &segments[idx];
        if seg.eq_ignore_ascii_case("anime") && segments.len() > 1 {
            continue;
        }
        picked.insert(0, seg.clone());
        if picked.len() >= 2 {
            break;
        }
    }

    if picked.is_empty() {
        if let Some(last) = segments.last() {
            picked.push(last.clone());
        }
    }

    let base = picked.join("-");
    let mut safe_base = sanitize_filename_component(&base);
    if safe_base.trim().is_empty() {
        safe_base = "animethemes".to_string();
    }
    output_dir.join(format!("{safe_base}-{timestamp}.mp4"))
}

fn sanitize_filename_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "animethemes".to_string()
    } else {
        out
    }
}

fn run_pipe_to_ffmpeg(
    mut producer: Command,
    ffmpeg: &Path,
    output_path: &Path,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    input_format: &str,
    tracker: &ProcessTracker,
) -> Result<(), String> {
    producer.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut producer_child = producer
        .spawn()
        .map_err(|err| format!("パイプライン起動に失敗しました: {err}"))?;
    tracker.register(&producer_child);

    if let Some(stderr) = producer_child.stderr.take() {
        let tx_stderr = tx.clone();
        let progress_ctx = progress.clone();
        thread::spawn(move || stream_lines(stderr, tx_stderr, progress_ctx));
    }

    let mut ffmpeg_cmd = Command::new(ffmpeg);
    ffmpeg_cmd
        .arg("-loglevel")
        .arg("error")
        .arg("-analyzeduration")
        .arg("100M")
        .arg("-probesize")
        .arg("100M")
        .arg("-f")
        .arg(input_format)
        .arg("-i")
        .arg("pipe:0")
        .arg("-c:v")
        .arg("h264_videotoolbox")
        .arg("-b:v")
        .arg("5M")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("192k")
        .arg("-ignore_unknown")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-f")
        .arg("mp4")
        .arg("-y")
        .arg(output_path.to_string_lossy().to_string())
        .stdin(
            producer_child
                .stdout
                .take()
                .ok_or_else(|| "パイプ入力の取得に失敗しました。".to_string())?,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut ffmpeg_child = ffmpeg_cmd
        .spawn()
        .map_err(|err| format!("ffmpeg起動に失敗しました: {err}"))?;
    tracker.register(&ffmpeg_child);

    if let Some(stdout) = ffmpeg_child.stdout.take() {
        let tx_stdout = tx.clone();
        let progress_ctx = progress.clone();
        thread::spawn(move || stream_lines(stdout, tx_stdout, progress_ctx));
    }

    if let Some(stderr) = ffmpeg_child.stderr.take() {
        let tx_stderr = tx.clone();
        let progress_ctx = progress.clone();
        thread::spawn(move || stream_lines(stderr, tx_stderr, progress_ctx));
    }

    let ffmpeg_status = ffmpeg_child
        .wait()
        .map_err(|err| format!("ffmpegの終了待ちに失敗しました: {err}"))?;
    let producer_status = producer_child
        .wait()
        .map_err(|err| format!("パイプライン終了待ちに失敗しました: {err}"))?;

    if !ffmpeg_status.success() {
        return Err(format!("ffmpegが異常終了しました: {ffmpeg_status}"));
    }
    if !producer_status.success() {
        return Err(format!("パイプラインが異常終了しました: {producer_status}"));
    }

    Ok(())
}

fn run_yt_dlp(
    yt_dlp_path: &Path,
    args: &[String],
    tx: &mpsc::Sender<DownloadEvent>,
    progress: Arc<ProgressContext>,
    add_bin_to_path: bool,
    tracker: &ProcessTracker,
) -> Result<std::process::ExitStatus, String> {
    let mut command = Command::new(yt_dlp_path);
    command.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    if add_bin_to_path {
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
    }

    let mut child = command
        .spawn()
        .map_err(|err| format!("yt-dlpの起動に失敗しました: {err}"))?;
    tracker.register(&child);

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

    let _ = tx.send(DownloadEvent::Log(trimmed.to_string()));
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
