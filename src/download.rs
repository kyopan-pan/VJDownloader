mod animethemes;
mod process;
mod staging;
mod tools;

use arboard::Clipboard;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::bundled::ensure_bundled_tools;
use crate::fs_utils::{ensure_dir, is_executable};
use crate::paths::{ffmpeg_path, yt_dlp_path};

pub use tools::{ensure_deno, ensure_yt_dlp, update_deno, update_yt_dlp};

pub enum DownloadEvent {
    Log(String),
    Progress(ProgressUpdate),
    Done(Result<(), String>),
}

pub(crate) const CANCELLED_ERROR: &str = "__CANCELLED__";

#[derive(Clone, Debug)]
pub struct ProgressUpdate {
    pub message: String,
    pub progress: f32,
    pub visible: bool,
}

impl ProgressUpdate {
    pub fn info_video_metadata(elapsed: &str) -> Self {
        Self {
            message: format!("動画情報確認中・・・{}", format_elapsed(elapsed)),
            progress: -1.0,
            visible: true,
        }
    }

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

    pub fn converting(percent: f32, elapsed: &str) -> Self {
        let clamped = percent.clamp(0.0, 100.0);
        Self {
            message: format!("変換中... {:.1}%{}", clamped, format_elapsed(elapsed)),
            progress: clamped / 100.0,
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

pub(super) struct ProgressContext {
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

    pub(super) fn elapsed(&self) -> String {
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

    pub(super) fn mark_progress_started(&self) {
        self.progress_started.store(true, Ordering::Relaxed);
    }

    fn progress_started(&self) -> bool {
        self.progress_started.load(Ordering::Relaxed)
    }

    pub(super) fn set_post_processing(&self) {
        self.post_processing.store(true, Ordering::Relaxed);
    }

    pub(super) fn post_processing(&self) -> bool {
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
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status();
        }
        for pid in &pids {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(pid.to_string())
                .status();
        }
    }
}

// ダウンロード処理のエントリポイント。進捗初期化から完了通知までを統括する。
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

// URL 判定と実体処理の振り分け、作業フォルダ後始末を行うメインフロー。
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

    // 必須ツールの存在確認を先に行う。
    ensure_bundled_tools()?;
    let ffmpeg = ffmpeg_path();
    if !ffmpeg.exists() {
        return Err("ffmpegが見つかりません。".to_string());
    }

    let yt_dlp_path = yt_dlp_path();
    if !yt_dlp_path.exists() || !is_executable(&yt_dlp_path) {
        return Err("yt-dlpが見つかりません。".to_string());
    }

    // 出力先と staging を作成する。
    if let Err(err) = ensure_dir(&output_dir) {
        return Err(format!("保存先フォルダの作成に失敗しました: {err}"));
    }
    let staging_dir = staging::create_download_staging_dir(&output_dir)?;

    // URL 種別ごとに処理を分岐する。
    let download_result = if is_animethemes_url(&url) {
        progress.mark_progress_started();
        let _ = tx.send(DownloadEvent::Progress(
            ProgressUpdate::info_video_metadata(&progress.elapsed()),
        ));
        animethemes::run_animethemes_pipeline(
            &url,
            &staging_dir,
            &yt_dlp_path,
            &ffmpeg,
            tx,
            progress,
            cancel_flag,
            tracker,
        )
    } else {
        let output_template = staging_dir.join("%(title)s.%(ext)s");
        let ffmpeg_arg = ffmpeg.to_string_lossy().to_string();

        let mut args = Vec::new();
        args.extend(tools::base_yt_dlp_args(&ffmpeg_arg, &cookie_args));
        args.push("-o".to_string());
        args.push(output_template.to_string_lossy().to_string());
        args.push(url.clone());

        let status = process::run_yt_dlp(&yt_dlp_path, &args, tx, progress.clone(), true, tracker);
        match status {
            Ok(code) if code.success() => Ok(()),
            Ok(_) => {
                let _ = tx.send(DownloadEvent::Log(
                    "H.264優先モードに失敗。互換モードで再試行します。".to_string(),
                ));
                if cancel_flag.load(Ordering::Relaxed) {
                    Err(CANCELLED_ERROR.to_string())
                } else {
                    let mut fallback_args = Vec::new();
                    fallback_args.extend(tools::fallback_yt_dlp_args(&ffmpeg_arg, &cookie_args));
                    fallback_args.push("-o".to_string());
                    fallback_args.push(output_template.to_string_lossy().to_string());
                    fallback_args.push(url);

                    let status = process::run_yt_dlp(
                        &yt_dlp_path,
                        &fallback_args,
                        tx,
                        progress.clone(),
                        true,
                        tracker,
                    );
                    if cancel_flag.load(Ordering::Relaxed) {
                        Err(CANCELLED_ERROR.to_string())
                    } else {
                        match status {
                            Ok(code) if code.success() => Ok(()),
                            Ok(code) => Err(format!("yt-dlp exited with status: {code}")),
                            Err(err) => Err(format!("yt-dlpの実行に失敗しました: {err}")),
                        }
                    }
                }
            }
            Err(err) => Err(format!("yt-dlpの実行に失敗しました: {err}")),
        }
    };

    // 成功時のみ staging 内 MP4 を昇格し、最後に staging を掃除する。
    let promote_result = match &download_result {
        Ok(()) => staging::promote_downloaded_mp4_files(&staging_dir, &output_dir),
        Err(_) => Ok(()),
    };
    let cleanup_error = fs::remove_dir_all(&staging_dir).err();

    if let Err(err) = promote_result {
        return Err(err);
    }
    if let Err(err) = download_result {
        return Err(err);
    }
    if let Some(err) = cleanup_error {
        return Err(format!("一時フォルダの削除に失敗しました: {err}"));
    }
    Ok(())
}

// クリップボード文字列を読み取り、空文字の場合は None を返す。
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

fn is_animethemes_url(url: &str) -> bool {
    url.to_lowercase().contains("animethemes.moe")
}

// 経過時間表示のフォーマットを統一する。
fn format_elapsed(elapsed: &str) -> String {
    if elapsed.trim().is_empty() {
        String::new()
    } else {
        format!(" (経過: {elapsed})")
    }
}

// 進捗率がまだ取れない初期フェーズの表示を定期更新する。
fn start_loading_elapsed_ticker(progress: Arc<ProgressContext>, tx: mpsc::Sender<DownloadEvent>) {
    thread::spawn(move || {
        while progress.is_active() && !progress.progress_started() {
            let update = ProgressUpdate::info_loading(&progress.elapsed());
            let _ = tx.send(DownloadEvent::Progress(update));
            thread::sleep(Duration::from_secs(1));
        }
    });
}

// 完了/失敗に応じて最終進捗状態を通知し、必要なら自動非表示を予約する。
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
