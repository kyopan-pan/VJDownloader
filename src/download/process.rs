use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use crate::paths::bin_dir;

use super::{CANCELLED_ERROR, DownloadEvent, ProcessTracker, ProgressContext, ProgressUpdate};

// 子プロセスを強制終了して wait まで行い、プロセスを確実に回収する。
pub(super) fn terminate_child_process(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

// producer -> ffmpeg のパイプラインを組み、MP4 へ変換する。
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

    spawn_stream_thread(producer_child.stderr.take(), tx, progress);

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

    spawn_stream_thread(ffmpeg_child.stdout.take(), tx, progress);
    spawn_stream_thread(ffmpeg_child.stderr.take(), tx, progress);

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

// パイプライン失敗時に、ユーザーキャンセルによる失敗かどうかを判定する。
pub(super) fn run_pipe_to_ffmpeg_or_cancel(
    producer: Command,
    ffmpeg: &Path,
    output_path: &Path,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    input_format: &str,
    tracker: &ProcessTracker,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(), String> {
    match run_pipe_to_ffmpeg(
        producer,
        ffmpeg,
        output_path,
        tx,
        progress,
        input_format,
        tracker,
    ) {
        Ok(()) => Ok(()),
        Err(err) => {
            if cancel_flag.load(Ordering::Relaxed) {
                Err(CANCELLED_ERROR.to_string())
            } else {
                Err(err)
            }
        }
    }
}

// yt-dlp を起動し、標準出力・標準エラーを並列で読み取って UI に流す。
pub(super) fn run_yt_dlp(
    yt_dlp_path: &Path,
    args: &[String],
    tx: &mpsc::Sender<DownloadEvent>,
    progress: Arc<ProgressContext>,
    add_bin_to_path: bool,
    tracker: &ProcessTracker,
) -> Result<std::process::ExitStatus, String> {
    let mut command = Command::new(yt_dlp_path);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

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

    spawn_stream_thread(child.stdout.take(), tx, &progress);
    spawn_stream_thread(child.stderr.take(), tx, &progress);

    child.wait().map_err(|err| err.to_string())
}

// 子プロセスのストリームを 1 行ずつ分解してログ・進捗イベントに変換する。
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

// Optional Reader を安全に監視スレッドへ渡すためのヘルパー。
pub(super) fn spawn_stream_thread<R: Read + Send + 'static>(
    reader: Option<R>,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
) {
    if let Some(reader) = reader {
        let tx_clone = tx.clone();
        let progress_clone = progress.clone();
        thread::spawn(move || stream_lines(reader, tx_clone, progress_clone));
    }
}

// 1 行ログを進捗解析し、その後 UI ログへ送る。
fn handle_stream_line(
    line: String,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    handle_progress_line(trimmed, progress, tx);

    let _ = tx.send(DownloadEvent::Log(trimmed.to_string()));
}

// yt-dlp/ffmpeg ログから進捗パーセンテージや変換フェーズ遷移を検出する。
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

// 1 行文字列内の "xx.x%" 形式を抽出する。
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

// ダウンロード完了後の後処理フェーズを示す行かどうかを判定する。
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
