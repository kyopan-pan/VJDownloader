pub(crate) mod animethemes;
mod guard;
mod process;
mod staging;
mod tools;

use arboard::Clipboard;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::bundled::ensure_bundled_tools;
use crate::fs_utils::{ensure_dir, is_executable};
use crate::paths::{ffmpeg_path, yt_dlp_path};

pub use guard::{BotGuardState, GuardNotice, is_youtube_url};
pub use tools::{ensure_deno, ensure_yt_dlp, js_runtime_arg, update_deno, update_yt_dlp};

pub enum DownloadEvent {
    Log(String),
    Progress(ProgressUpdate),
    // Bot対策（403/429 や待機）を検出したときの通知。
    Guard(GuardNotice),
    Done(Result<(), String>, String),
}

// ダウンロード仕様の選択肢。設定画面で切り替える。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DownloadMode {
    // 現状の仕様。H.264優先モードで取得し、失敗した場合のみ互換モード（720p上限）へ切り替える。
    #[default]
    Standard,
    // 1080p上限で取得する。1080p未満が最高画質の場合はその画質を使う。
    UpTo1080p,
    // 最高画質で取得した後、既定フォーマット（H.264 MP4）へ変換する。
    BestThenConvert,
}

impl DownloadMode {
    pub const ALL: [DownloadMode; 3] = [
        DownloadMode::Standard,
        DownloadMode::UpTo1080p,
        DownloadMode::BestThenConvert,
    ];

    // 設定ファイルへ保存する識別子。
    pub fn as_key(self) -> &'static str {
        match self {
            DownloadMode::Standard => "standard",
            DownloadMode::UpTo1080p => "up_to_1080p",
            DownloadMode::BestThenConvert => "best_then_convert",
        }
    }

    // 設定ファイルの値から復元する。未知の値は None を返す。
    pub fn from_key(raw: &str) -> Option<Self> {
        let key = raw.trim();
        Self::ALL
            .into_iter()
            .find(|mode| key.eq_ignore_ascii_case(mode.as_key()))
    }

    pub fn label(self) -> &'static str {
        match self {
            DownloadMode::Standard => "標準（H.264優先）",
            DownloadMode::UpTo1080p => "1080p上限",
            DownloadMode::BestThenConvert => "最高画質 + 変換",
        }
    }

    // ホバー時の吹き出しに表示する仕様の説明。
    pub fn description(self) -> &'static str {
        match self {
            DownloadMode::Standard => {
                "H.264を優先し、解像度の上限を設けずに取得します。H.264で取得できなかった場合のみ、\
                 720p上限の互換モードへ切り替えて再変換します。"
            }
            DownloadMode::UpTo1080p => {
                "1080pを上限にH.264を優先して取得します。1080p未満が最高画質の場合はその画質を使います。\
                 失敗した場合は1080p上限の互換モードで再試行します。"
            }
            DownloadMode::BestThenConvert => {
                "コーデックを問わず最高画質で取得し、そのあと既定フォーマット（H.264 MP4）へ変換します。\
                 VP9やAV1の高解像度素材もVDMXで扱えるMP4になります。"
            }
        }
    }

    // 説明に添える注意書き。特に注意が要らない仕様は None を返す。
    pub fn caution(self) -> Option<&'static str> {
        match self {
            DownloadMode::Standard => None,
            DownloadMode::UpTo1080p => Some("元が1080pより高画質でも1080pに落ちます。"),
            DownloadMode::BestThenConvert => Some(
                "ダウンロード後に必ず変換が走るため時間がかかります。変換は5Mbps固定のため、\
                 4Kなどの高解像度素材では画質が伸びない場合があります。",
            ),
        }
    }
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
    // terminate_all 呼び出し済みかどうか。以降に register された子プロセスも確実に止めるためのラッチ。
    terminated: Arc<AtomicBool>,
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
        let already_terminated = {
            let mut pids = self.pids.lock().unwrap();
            if !pids.contains(&pid) {
                pids.push(pid);
            }
            self.terminated.load(Ordering::Relaxed)
        };
        // terminate_all のスナップショットから漏れた子プロセスはここで止める。
        if already_terminated {
            terminate_pids(vec![pid]);
        }
    }

    pub fn unregister(&self, pid: u32) {
        if pid == 0 {
            return;
        }
        let mut pids = self.pids.lock().unwrap();
        pids.retain(|tracked| *tracked != pid);
    }

    pub fn terminate_all(&self) {
        let pids = {
            let pids = self.pids.lock().unwrap();
            self.terminated.store(true, Ordering::Relaxed);
            pids.clone()
        };
        terminate_pids(pids);
    }
}

// 指定した pid 群へ終了シグナルを送る。
fn terminate_pids(pids: Vec<u32>) {
    // 一時停止（SIGSTOP）中だと SIGTERM が配送されず終了処理が走らないため、
    // 先に SIGCONT で再開させてから SIGTERM で穏やかに終了を促す。ffmpeg はこの過程で
    // audiotoolbox 出力のオーディオキューを正常に停止・破棄するため、停止直後の音声ループを防げる。
    for pid in &pids {
        let _ = Command::new("kill")
            .arg("-CONT")
            .arg(pid.to_string())
            .status();
    }
    for pid in &pids {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
    }
    // SIGTERM 後の終了確認と SIGKILL での後始末は別スレッドで行い、呼び出し元（UI スレッド等）を
    // ブロックしない。ffmpeg は SIGTERM 受信後すぐにオーディオキューを破棄して終了するため、
    // 強制終了が必要になるのは応答しないプロセスだけ。
    thread::spawn(move || {
        for pid in &pids {
            if !wait_for_exit(*pid, Duration::from_millis(2000)) {
                let _ = Command::new("kill")
                    .arg("-KILL")
                    .arg(pid.to_string())
                    .status();
            }
        }
    });
}

// 指定 pid が終了するまで最大 timeout だけ待つ。終了を確認できたら true を返す。
// `kill -0` でプロセスの生存を確認する（シグナルは送らず存在判定のみ）。
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let alive = Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !alive {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

// ダウンロード処理のエントリポイント。進捗初期化から完了通知までを統括する。
pub fn run_download(
    url: String,
    output_dir: PathBuf,
    cookie_args: Vec<String>,
    mode: DownloadMode,
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
        mode,
        &tx,
        &progress,
        &cancel_flag,
        &tracker,
    );

    let total_elapsed = progress.elapsed();
    finalize_progress(&progress, &tx, result.is_ok());
    let _ = tx.send(DownloadEvent::Done(result, total_elapsed));
}

// URL 判定と実体処理の振り分け、作業フォルダ後始末を行うメインフロー。
fn run_download_inner(
    url: String,
    output_dir: PathBuf,
    cookie_args: Vec<String>,
    mode: DownloadMode,
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
        let _ = tx.send(DownloadEvent::Log(format!(
            "ダウンロード仕様: {}",
            mode.label()
        )));
        run_yt_dlp_download(
            &url,
            &staging_dir,
            &yt_dlp_path,
            &ffmpeg,
            &cookie_args,
            mode,
            tx,
            progress,
            cancel_flag,
            tracker,
        )
    };

    // 成功時のみ staging 内 MP4 を昇格し、最後に staging を掃除する。
    let promote_result = match &download_result {
        Ok(()) => staging::promote_downloaded_mp4_files(&staging_dir, &output_dir),
        Err(_) => Ok(()),
    };
    let cleanup_error = fs::remove_dir_all(&staging_dir).err();

    promote_result?;
    download_result?;
    if let Some(err) = cleanup_error {
        return Err(format!("一時フォルダの削除に失敗しました: {err}"));
    }
    Ok(())
}

// 選択中のダウンロード仕様に沿って yt-dlp を実行し、必要なら後処理の変換まで行う。
#[allow(clippy::too_many_arguments)]
fn run_yt_dlp_download(
    url: &str,
    staging_dir: &Path,
    yt_dlp_path: &Path,
    ffmpeg: &Path,
    cookie_args: &[String],
    mode: DownloadMode,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    cancel_flag: &Arc<AtomicBool>,
    tracker: &ProcessTracker,
) -> Result<(), String> {
    let output_template = staging_dir.join("%(title)s.%(ext)s");
    let ffmpeg_arg = ffmpeg.to_string_lossy().to_string();
    let js_runtime = tools::js_runtime_arg();

    let run = |args: Vec<String>| -> Result<std::process::ExitStatus, String> {
        let mut args = args;
        args.push("-o".to_string());
        args.push(output_template.to_string_lossy().to_string());
        args.push(url.to_string());
        process::run_yt_dlp(yt_dlp_path, &args, tx, progress.clone(), true, tracker)
            .map_err(|err| format!("yt-dlpの実行に失敗しました: {err}"))
    };

    let status = run(tools::base_yt_dlp_args(
        mode,
        &ffmpeg_arg,
        cookie_args,
        &js_runtime,
    ))?;

    if !status.success() {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err(CANCELLED_ERROR.to_string());
        }
        let Some(fallback_args) =
            tools::fallback_yt_dlp_args(mode, &ffmpeg_arg, cookie_args, &js_runtime)
        else {
            return Err(format!("yt-dlp exited with status: {status}"));
        };

        let _ = tx.send(DownloadEvent::Log(
            "H.264優先モードに失敗。互換モードで再試行します。".to_string(),
        ));
        // キャンセル中は実行エラーよりキャンセルを優先して報告する。
        let status = run(fallback_args);
        if cancel_flag.load(Ordering::Relaxed) {
            return Err(CANCELLED_ERROR.to_string());
        }
        let status = status?;
        if !status.success() {
            return Err(format!("yt-dlp exited with status: {status}"));
        }
    }

    if mode == DownloadMode::BestThenConvert {
        convert_staged_files_to_default_format(
            staging_dir,
            ffmpeg,
            tx,
            progress,
            cancel_flag,
            tracker,
        )?;
    }

    Ok(())
}

// 最高画質で取得したファイルを、既定フォーマット（H.264 MP4）へ順番に変換する。
fn convert_staged_files_to_default_format(
    staging_dir: &Path,
    ffmpeg: &Path,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    cancel_flag: &Arc<AtomicBool>,
    tracker: &ProcessTracker,
) -> Result<(), String> {
    let targets = staging::collect_convertible_files(staging_dir)?;
    if targets.is_empty() {
        return Err("ダウンロードしたファイルが見つかりませんでした。".to_string());
    }

    progress.mark_progress_started();
    progress.set_post_processing();
    start_post_processing_elapsed_ticker(progress.clone(), tx.clone());

    for source in targets {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err(CANCELLED_ERROR.to_string());
        }
        let _ = tx.send(DownloadEvent::Log(format!(
            "既定フォーマットへ変換します: {}",
            source.file_name().unwrap_or_default().to_string_lossy()
        )));
        let temporary = staging::default_format_temp_path(&source);
        let result = process::run_default_format_convert(
            ffmpeg,
            &source,
            &temporary,
            tx,
            progress,
            tracker,
            cancel_flag,
        );
        if let Err(err) = result {
            let _ = fs::remove_file(&temporary);
            return Err(err);
        }
        staging::replace_with_converted_mp4(&source, &temporary)?;
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

// 変換フェーズ中の経過時間表示を定期更新する。
fn start_post_processing_elapsed_ticker(
    progress: Arc<ProgressContext>,
    tx: mpsc::Sender<DownloadEvent>,
) {
    thread::spawn(move || {
        while progress.is_active() && progress.post_processing() {
            let update = ProgressUpdate::post_processing(&progress.elapsed());
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
