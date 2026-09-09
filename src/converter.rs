use eframe::egui;
use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::paths::ffmpeg_path;
use crate::theme::paint_viewport_background;

#[derive(Debug)]
pub enum ConversionEvent {
    Completed(PathBuf),
    Failed(String),
    Log(String),
}

struct FfmpegOutput {
    status: ExitStatus,
    stderr: String,
}

#[derive(Default)]
struct ConverterUiState {
    busy: bool,
    current_file: Option<String>,
    completed_count: usize,
    total_count: usize,
    message: String,
    last_output: Option<PathBuf>,
    error: Option<String>,
}

pub struct ConverterUiHandle {
    state: Arc<Mutex<ConverterUiState>>,
    visible: Arc<AtomicBool>,
    output_dir: Arc<Mutex<PathBuf>>,
    event_tx: mpsc::Sender<ConversionEvent>,
    event_rx: mpsc::Receiver<ConversionEvent>,
}

impl ConverterUiHandle {
    pub fn new(output_dir: PathBuf) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        Self {
            state: Arc::new(Mutex::new(ConverterUiState {
                message: "動画ファイルをドロップしてください".to_string(),
                ..Default::default()
            })),
            visible: Arc::new(AtomicBool::new(false)),
            output_dir: Arc::new(Mutex::new(output_dir)),
            event_tx,
            event_rx,
        }
    }

    pub fn open(&self) {
        self.visible.store(true, Ordering::Release);
    }

    pub fn set_output_dir(&self, output_dir: PathBuf) {
        if let Ok(mut current) = self.output_dir.lock() {
            *current = output_dir;
        }
    }

    pub fn try_recv_event(&self) -> Result<ConversionEvent, mpsc::TryRecvError> {
        self.event_rx.try_recv()
    }
}

pub fn render_converter_viewport(handle: &ConverterUiHandle, ctx: &egui::Context) {
    if !handle.visible.load(Ordering::Acquire) {
        return;
    }

    let viewport_id = egui::ViewportId::from_hash_of("video-converter-window");
    let builder = egui::ViewportBuilder::default()
        .with_title("動画をMP4に変換")
        .with_inner_size(egui::vec2(520.0, 390.0))
        .with_min_inner_size(egui::vec2(440.0, 320.0))
        .with_always_on_top();

    let state = Arc::clone(&handle.state);
    let visible = Arc::clone(&handle.visible);
    let output_dir = Arc::clone(&handle.output_dir);
    let event_tx = handle.event_tx.clone();
    ctx.show_viewport_deferred(viewport_id, builder, move |ui, _class| {
        paint_viewport_background(ui);
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            visible.store(false, Ordering::Release);
            return;
        }

        let hovered = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let dropped = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect::<Vec<_>>()
        });

        render_contents(ui, &state, hovered);

        if !dropped.is_empty() {
            start_conversion_batch(
                dropped,
                Arc::clone(&state),
                Arc::clone(&output_dir),
                event_tx.clone(),
                ui.ctx().clone(),
                viewport_id,
            );
        }

        let busy = state.lock().map(|state| state.busy).unwrap_or(false);
        if busy || hovered {
            ui.ctx().request_repaint_of(viewport_id);
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    });
}

fn render_contents(ui: &mut egui::Ui, state: &Arc<Mutex<ConverterUiState>>, hovered: bool) {
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(20, 18))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("動画をMP4に変換")
                    .size(20.0)
                    .strong()
                    .color(egui::Color32::from_rgb(226, 232, 240)),
            );
            ui.label(
                egui::RichText::new("WebM、MOV、MKVなどをffmpegでMP4へ変換します。")
                    .size(12.0)
                    .color(egui::Color32::from_rgb(140, 150, 170)),
            );
            ui.add_space(18.0);

            let Ok(state) = state.lock() else { return };
            let drop_fill = if hovered && !state.busy {
                egui::Color32::from_rgb(20, 55, 74)
            } else {
                egui::Color32::from_rgb(20, 28, 44)
            };
            let drop_stroke = if hovered && !state.busy {
                egui::Stroke::new(2.0, egui::Color32::from_rgb(16, 190, 255))
            } else {
                egui::Stroke::new(1.0, egui::Color32::from_rgb(52, 64, 86))
            };

            egui::Frame::NONE
                .fill(drop_fill)
                .stroke(drop_stroke)
                .corner_radius(egui::CornerRadius::same(16))
                .inner_margin(egui::Margin::symmetric(20, 36))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.vertical_centered(|ui| {
                        if state.busy {
                            ui.add(egui::Spinner::new().size(30.0));
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("変換中...")
                                    .size(16.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(16, 190, 255)),
                            );
                            if let Some(file) = &state.current_file {
                                ui.label(
                                    egui::RichText::new(file)
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(190, 200, 220)),
                                );
                            }
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} / {}",
                                    state.completed_count + 1,
                                    state.total_count
                                ))
                                .size(11.5)
                                .color(egui::Color32::from_rgb(130, 140, 160)),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(if hovered {
                                    "ここにドロップして変換"
                                } else {
                                    "動画ファイルをここにドラッグ＆ドロップ"
                                })
                                .size(15.0)
                                .strong()
                                .color(egui::Color32::from_rgb(220, 230, 245)),
                            );
                            ui.label(
                                egui::RichText::new("複数ファイルをまとめてドロップできます")
                                    .size(11.5)
                                    .color(egui::Color32::from_rgb(130, 140, 160)),
                            );
                        }
                    });
                });

            ui.add_space(14.0);
            let status_color = if state.error.is_some() {
                egui::Color32::from_rgb(248, 113, 113)
            } else if state.last_output.is_some() && !state.busy {
                egui::Color32::from_rgb(52, 211, 153)
            } else {
                egui::Color32::from_rgb(160, 170, 190)
            };
            ui.label(
                egui::RichText::new(&state.message)
                    .size(12.0)
                    .color(status_color),
            );
            if let Some(path) = &state.last_output {
                ui.label(
                    egui::RichText::new(path.to_string_lossy())
                        .size(11.0)
                        .color(egui::Color32::from_rgb(120, 130, 150)),
                );
            }
        });
}

fn start_conversion_batch(
    paths: Vec<PathBuf>,
    state: Arc<Mutex<ConverterUiState>>,
    output_dir: Arc<Mutex<PathBuf>>,
    event_tx: mpsc::Sender<ConversionEvent>,
    ctx: egui::Context,
    viewport_id: egui::ViewportId,
) {
    let paths = paths
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        if let Ok(mut state) = state.lock() {
            state.error = Some("動画ファイルをドロップしてください。".to_string());
            state.message = "有効なファイルが見つかりませんでした。".to_string();
        }
        return;
    }

    {
        let Ok(mut state) = state.lock() else { return };
        if state.busy {
            state.error =
                Some("変換が終わってから次のファイルをドロップしてください。".to_string());
            state.message = "現在、別の変換を実行中です。".to_string();
            return;
        }
        state.busy = true;
        state.completed_count = 0;
        state.total_count = paths.len();
        state.last_output = None;
        state.error = None;
        state.message = "変換を開始しています...".to_string();
    }

    thread::spawn(move || {
        let target_dir = output_dir
            .lock()
            .map(|dir| dir.clone())
            .unwrap_or_else(|_| PathBuf::from("."));
        let total = paths.len();
        let mut queue = VecDeque::from(paths);
        let mut completed = 0usize;
        let mut last_output = None;
        let mut last_error = None;

        while let Some(input) = queue.pop_front() {
            if let Ok(mut state) = state.lock() {
                state.current_file = input
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string());
                state.completed_count = completed;
                state.message = "ffmpegで変換しています...".to_string();
            }
            ctx.request_repaint_of(viewport_id);

            let _ = event_tx.send(ConversionEvent::Log(format!(
                "MP4変換を開始します: {}",
                input.to_string_lossy()
            )));
            ctx.request_repaint_of(egui::ViewportId::ROOT);

            match convert_to_mp4(&input, &target_dir, &event_tx, &ctx) {
                Ok(output) => {
                    completed += 1;
                    last_output = Some(output.clone());
                    let _ = event_tx.send(ConversionEvent::Completed(output));
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
                Err(error) => {
                    let message = format!(
                        "{}: {error}",
                        input
                            .file_name()
                            .unwrap_or(input.as_os_str())
                            .to_string_lossy()
                    );
                    last_error = Some(message.clone());
                    let _ = event_tx.send(ConversionEvent::Failed(message));
                }
            }
        }

        if let Ok(mut state) = state.lock() {
            state.busy = false;
            state.current_file = None;
            state.completed_count = completed;
            state.total_count = total;
            state.last_output = last_output;
            state.error = last_error.clone();
            state.message = match last_error {
                Some(_) => format!("{completed} / {total} ファイルの変換が完了しました。"),
                None => format!("{completed} ファイルの変換が完了しました。"),
            };
        }
        ctx.request_repaint_of(viewport_id);
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    });
}

fn convert_to_mp4(
    input: &Path,
    output_dir: &Path,
    event_tx: &mpsc::Sender<ConversionEvent>,
    ctx: &egui::Context,
) -> Result<PathBuf, String> {
    fs::create_dir_all(output_dir)
        .map_err(|error| format!("出力先フォルダを作成できませんでした: {error}"))?;

    let ffmpeg = ffmpeg_path();
    if !ffmpeg.is_file() {
        return Err("ffmpegが見つかりません。アプリを再起動してください。".to_string());
    }

    let output = unique_output_path(input, output_dir);
    let file_name = output
        .file_name()
        .ok_or_else(|| "出力ファイル名を作成できませんでした。".to_string())?;
    let temporary = output_dir.join(format!(".{}.converting", file_name.to_string_lossy()));

    let _ = event_tx.send(ConversionEvent::Log(
        LOG_CONVERT_WITH_VIDEOTOOLBOX.to_string(),
    ));
    let mut result = run_ffmpeg_conversion(&ffmpeg, input, &temporary, true, event_tx, ctx)?;
    if !result.status.success() && libx264_retry_available() {
        let _ = fs::remove_file(&temporary);
        let _ = event_tx.send(ConversionEvent::Log(LOG_RETRY_WITH_LIBX264.to_string()));
        ctx.request_repaint_of(egui::ViewportId::ROOT);
        result = run_ffmpeg_conversion(&ffmpeg, input, &temporary, false, event_tx, ctx)?;
    }

    if !result.status.success() {
        let _ = fs::remove_file(&temporary);
        let detail = result.stderr.trim();
        return Err(if detail.is_empty() {
            format!("ffmpegが終了コード{}で失敗しました。", result.status)
        } else {
            format!("ffmpegの変換に失敗しました: {}", truncate_error(detail))
        });
    }

    fs::rename(&temporary, &output).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("変換済みファイルを保存できませんでした: {error}")
    })?;
    Ok(output)
}

// 変換開始とエンコーダ切り替えのログ文言。変換ウィンドウとダウンロード後の変換で共有する。
pub(crate) const LOG_CONVERT_WITH_VIDEOTOOLBOX: &str = "ffmpeg: h264_videotoolboxで変換します。";
pub(crate) const LOG_RETRY_WITH_LIBX264: &str =
    "ffmpeg: VideoToolboxを利用できないためlibx264で再試行します。";

// 変換に使う H.264 エンコーダ名。h264_videotoolbox は macOS 専用のため、
// それ以外のプラットフォームでは libx264 を使用する。
pub(crate) fn h264_encoder() -> &'static str {
    if cfg!(target_os = "macos") {
        "h264_videotoolbox"
    } else {
        "libx264"
    }
}

// VideoToolbox が失敗したときに libx264 で再試行できるか。
// macOS の同梱 ffmpeg は libx264 を含まない LGPL ビルドのため再試行しない。
// 対応環境が Apple Silicon 限定で VideoToolbox は常に利用できるため、
// 再試行しても「Unknown encoder」で失敗し、原因を隠すだけになる。
// Windows は公式ビルドの ffmpeg を取得するため libx264 を使用できる。
pub(crate) fn libx264_retry_available() -> bool {
    !cfg!(target_os = "macos")
}

// 既定フォーマット（H.264 MP4）へ変換する ffmpeg コマンドを組み立てる。
// 変換ウィンドウとダウンロード後の変換で同じ設定を共有する。
pub(crate) fn default_mp4_command(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    use_videotoolbox: bool,
) -> Command {
    let mut command = Command::new(ffmpeg);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .args(["-loglevel", "info", "-stats_period", "0.5"])
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0?", "-map", "0:a?"]);
    if use_videotoolbox {
        command.args(["-c:v", "h264_videotoolbox", "-allow_sw", "1"]);
    } else {
        command.args(["-c:v", "libx264"]);
    }
    command
        .args(["-b:v", "5M", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", "192k"])
        .args(["-movflags", "+faststart", "-f", "mp4", "-y"])
        .arg(output);
    command
}

fn run_ffmpeg_conversion(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    use_videotoolbox: bool,
    event_tx: &mpsc::Sender<ConversionEvent>,
    ctx: &egui::Context,
) -> Result<FfmpegOutput, String> {
    let mut command = default_mp4_command(ffmpeg, input, output, use_videotoolbox);
    command.stdout(Stdio::null()).stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|error| format!("ffmpegを起動できませんでした: {error}"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "ffmpegのログ出力を取得できませんでした。".to_string())?;
    let mut pending = Vec::new();
    let mut pump = LogPump::default();
    let mut buffer = [0u8; 4096];

    loop {
        let read = stderr
            .read(&mut buffer)
            .map_err(|error| format!("ffmpegのログ読み取りに失敗しました: {error}"))?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..read]);
        drain_log_lines(&mut pending, false, event_tx, ctx, &mut pump);
    }
    drain_log_lines(&mut pending, true, event_tx, ctx, &mut pump);

    let status = child
        .wait()
        .map_err(|error| format!("ffmpegの終了状態を取得できませんでした: {error}"))?;
    Ok(FfmpegOutput {
        status,
        stderr: pump.error_text,
    })
}

/// ffmpeg の出力を読み進めるあいだの状態。エラー本文の蓄積と stats 行の間引きを持つ。
#[derive(Default)]
struct LogPump {
    error_text: String,
    last_stats_at: Option<Instant>,
}

impl LogPump {
    /// stats 行をログへ流すかを判定する。`-stats_period 0.5` の生出力は毎秒2件になり、
    /// そのまま出すとログ画面の保持件数を数分で使い切って他の記録を押し出してしまう。
    fn allow_stats_line(&mut self) -> bool {
        let now = Instant::now();
        let allow = self
            .last_stats_at
            .is_none_or(|last| now.duration_since(last) >= STATS_LOG_INTERVAL);
        if allow {
            self.last_stats_at = Some(now);
        }
        allow
    }
}

/// ffmpeg の stats 行をログへ出す最短間隔。進捗の手応えは残しつつ件数を抑える。
const STATS_LOG_INTERVAL: Duration = Duration::from_secs(5);

/// ffmpeg の stats 行（`frame=... time=...`）かどうか。
fn is_stats_line(line: &str) -> bool {
    line.starts_with("frame=") || line.starts_with("size=")
}

fn drain_log_lines(
    pending: &mut Vec<u8>,
    flush: bool,
    event_tx: &mpsc::Sender<ConversionEvent>,
    ctx: &egui::Context,
    pump: &mut LogPump,
) {
    loop {
        let delimiter = pending
            .iter()
            .position(|byte| *byte == b'\n' || *byte == b'\r');
        let Some(end) = delimiter else {
            if flush && !pending.is_empty() {
                emit_ffmpeg_log(pending, event_tx, ctx, pump);
                pending.clear();
            }
            return;
        };

        let line = pending[..end].to_vec();
        let mut consumed = end + 1;
        while consumed < pending.len() && (pending[consumed] == b'\n' || pending[consumed] == b'\r')
        {
            consumed += 1;
        }
        pending.drain(..consumed);
        emit_ffmpeg_log(&line, event_tx, ctx, pump);
    }
}

fn emit_ffmpeg_log(
    bytes: &[u8],
    event_tx: &mpsc::Sender<ConversionEvent>,
    ctx: &egui::Context,
    pump: &mut LogPump,
) {
    let line = String::from_utf8_lossy(bytes).trim().to_string();
    if line.is_empty() {
        return;
    }

    // 失敗時のエラー本文は間引かず全行を蓄える。原因はstats行の直後に出ることがある。
    const MAX_ERROR_CHARS: usize = 16_000;
    if pump.error_text.chars().count() < MAX_ERROR_CHARS {
        if !pump.error_text.is_empty() {
            pump.error_text.push('\n');
        }
        pump.error_text.push_str(&line);
    }

    if is_stats_line(&line) && !pump.allow_stats_line() {
        return;
    }
    let _ = event_tx.send(ConversionEvent::Log(format!("[ffmpeg] {line}")));
    ctx.request_repaint_of(egui::ViewportId::ROOT);
}

fn unique_output_path(input: &Path, output_dir: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .unwrap_or(input.as_os_str())
        .to_string_lossy();
    let first = output_dir.join(format!("{stem}.mp4"));
    if !first.exists() {
        return first;
    }

    let converted = output_dir.join(format!("{stem}-converted.mp4"));
    if !converted.exists() {
        return converted;
    }

    for suffix in 2.. {
        let candidate = output_dir.join(format!("{stem}-converted-{suffix}.mp4"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

pub(crate) fn truncate_error(error: &str) -> String {
    const MAX_CHARS: usize = 500;
    let mut chars = error.chars();
    let text = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{text}...")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_ffmpeg_stats_lines() {
        assert!(is_stats_line(
            "frame=  120 fps= 59 q=-1.0 size=1024kB time=00:00:02.00 bitrate=4096.0kbits/s"
        ));
        assert!(is_stats_line(
            "size=  512kB time=00:00:01.00 bitrate=1.0kbits/s"
        ));
        assert!(
            !is_stats_line("[libx264 @ 0x7f8] using SAR=1/1"),
            "通常のログ行をstats行と誤判定している"
        );
        assert!(
            !is_stats_line("Output #0, mp4, to '/tmp/out.mp4':"),
            "通常のログ行をstats行と誤判定している"
        );
    }

    #[test]
    fn stats_lines_are_throttled_but_error_text_keeps_every_line() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut pump = LogPump::default();

        // `-stats_period 0.5` を模して連続で流し込む。間隔を空けないので1件だけ通るのが正しい。
        for index in 0..5 {
            let line = format!("frame= {index} fps=59 time=00:00:0{index}.00");
            emit_ffmpeg_log(line.as_bytes(), &tx, &ctx, &mut pump);
        }
        // stats行でない行は間引かない。
        emit_ffmpeg_log(b"Output #0, mp4, to '/tmp/out.mp4':", &tx, &ctx, &mut pump);

        let logged = rx.try_iter().count();
        assert_eq!(logged, 2, "stats行の間引きが効いていない: {logged}件");
        // 失敗時の原因調査に使うエラー本文は間引かず全行を保持する。
        assert_eq!(
            pump.error_text.lines().count(),
            6,
            "エラー本文から行が失われている"
        );
    }
}
