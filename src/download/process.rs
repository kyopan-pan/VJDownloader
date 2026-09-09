use std::fs;
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use crate::converter::{
    LOG_CONVERT_WITH_VIDEOTOOLBOX, LOG_RETRY_WITH_LIBX264, default_mp4_command, h264_encoder,
    libx264_retry_available, truncate_error,
};
use crate::paths::bin_dir;

use super::guard;
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
        .arg(h264_encoder())
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

// ダウンロード済みファイルを既定フォーマット（H.264 MP4）へ変換する。
// libx264 が使える環境（Windows）では、VideoToolbox が失敗したときに libx264 で再試行する。
#[allow(clippy::too_many_arguments)]
pub(super) fn run_default_format_convert(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    tracker: &ProcessTracker,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(), String> {
    crate::log_info!(Download, "{LOG_CONVERT_WITH_VIDEOTOOLBOX}");
    let (status, stderr) = run_convert_command(ffmpeg, input, output, true, tx, progress, tracker)?;
    if status.success() {
        return Ok(());
    }
    if cancel_flag.load(Ordering::Relaxed) {
        return Err(CANCELLED_ERROR.to_string());
    }

    // libx264 が使えない環境では再試行せず、VideoToolbox の失敗をそのまま報告する。
    let (status, stderr) = if libx264_retry_available() {
        let _ = fs::remove_file(output);
        crate::log_warn!(Download, "{LOG_RETRY_WITH_LIBX264}");
        let retried = run_convert_command(ffmpeg, input, output, false, tx, progress, tracker)?;
        if cancel_flag.load(Ordering::Relaxed) {
            return Err(CANCELLED_ERROR.to_string());
        }
        retried
    } else {
        (status, stderr)
    };
    if !status.success() {
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            format!("ffmpegが終了コード{status}で失敗しました。")
        } else {
            format!("ffmpegの変換に失敗しました: {}", truncate_error(detail))
        });
    }
    Ok(())
}

// 変換用 ffmpeg を 1 回実行し、ログを UI へ流しながら終了を待つ。
// 失敗時のエラーメッセージ用に stderr の内容も返す。
fn run_convert_command(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    use_videotoolbox: bool,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    tracker: &ProcessTracker,
) -> Result<(std::process::ExitStatus, String), String> {
    let mut command = default_mp4_command(ffmpeg, input, output, use_videotoolbox);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|err| format!("ffmpeg起動に失敗しました: {err}"))?;
    tracker.register(&child);

    spawn_stream_thread(child.stdout.take(), tx, progress);
    let stderr_thread = spawn_capture_stream_thread(child.stderr.take(), tx, progress);

    let status = child
        .wait()
        .map_err(|err| format!("ffmpegの終了待ちに失敗しました: {err}"))?;
    let stderr = stderr_thread
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    Ok((status, stderr))
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
        let mut paths = Vec::new();
        let bin = bin_dir();
        if bin.exists() {
            paths.push(bin.as_os_str().to_owned());
        }
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
    tracker.register(&child);

    spawn_stream_thread(child.stdout.take(), tx, &progress);
    spawn_stream_thread(child.stderr.take(), tx, &progress);

    child.wait().map_err(|err| err.to_string())
}

// 子プロセスのストリームを 1 行ずつ分解してログ・進捗イベントに変換する。
// capture を渡すと、流した行の全文をそこへ蓄積する。
fn stream_lines<R: Read + Send + 'static>(
    reader: R,
    tx: mpsc::Sender<DownloadEvent>,
    progress: Arc<ProgressContext>,
    mut capture: Option<&mut String>,
) {
    let mut buffered = BufReader::new(reader);
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    let emit = |line: &[u8], capture: &mut Option<&mut String>| {
        let text = match String::from_utf8(line.to_vec()) {
            Ok(text) => text,
            Err(_) => String::from_utf8_lossy(line).to_string(),
        };
        if let Some(capture) = capture {
            capture.push_str(&text);
            capture.push('\n');
        }
        handle_stream_line(text, &tx, &progress);
    };
    loop {
        let read = match buffered.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        for &byte in &buf[..read] {
            if byte == b'\n' || byte == b'\r' {
                if !line.is_empty() {
                    emit(&line, &mut capture);
                    line.clear();
                }
            } else {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        emit(&line, &mut capture);
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
        thread::spawn(move || stream_lines(reader, tx_clone, progress_clone, None));
    }
}

// UI ログへ流しつつ全文を蓄積するヘルパー。join すると蓄積した内容を得られる。
fn spawn_capture_stream_thread<R: Read + Send + 'static>(
    reader: Option<R>,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
) -> Option<thread::JoinHandle<String>> {
    let reader = reader?;
    let tx = tx.clone();
    let progress = progress.clone();
    Some(thread::spawn(move || {
        let mut captured = String::new();
        stream_lines(reader, tx, progress, Some(&mut captured));
        captured
    }))
}

// 1 行ログを進捗解析し、yt-dlp の生進捗だけを簡潔なログへ集約して UI へ送る。
fn handle_stream_line(
    line: String,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    let download_percent = yt_dlp_download_percent(trimmed);
    handle_progress_line(trimmed, progress, tx);
    guard::notify(trimmed, tx);

    // 強制終了させた子プロセスが吐く進捗と終了エラーは記録しない。進捗バーの更新
    // （handle_progress_line）はキャンセル表示のために続ける。
    if progress.is_cancelling() {
        return;
    }

    if let Some(percent) = download_percent {
        if let Some(percent) = progress.next_log_percent(percent) {
            crate::log_info!(Download, "ダウンロード進捗: {percent}%");
        }
        return;
    }

    // yt-dlp / ffmpeg の生出力なので、行の慣習からレベルを推定する。
    crate::logs::emit(
        crate::logs::classify_tool_line(trimmed),
        crate::logs::Source::Download,
        trimmed,
    );
}

// 保存先などの通知行は除外し、yt-dlp の `[download] xx.x%` 行だけを判定する。
// ファイル名に `%` を含む通知行を進捗と誤認しないよう、タグ直後の数値だけを見る。
fn yt_dlp_download_percent(line: &str) -> Option<f32> {
    let rest = line.strip_prefix("[download]")?.trim_start();
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with('%') {
        return None;
    }
    digits.parse::<f32>().ok()
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

#[cfg(test)]
mod tests {
    use super::yt_dlp_download_percent;

    #[test]
    fn recognizes_only_yt_dlp_percentage_progress_lines() {
        assert_eq!(
            yt_dlp_download_percent("[download]  34.5% of 85.87MiB"),
            Some(34.5)
        );
        assert_eq!(
            yt_dlp_download_percent("[download] Destination: 日本語.mp4"),
            None
        );
        assert_eq!(
            yt_dlp_download_percent("[Merger] Merging formats into 日本語.mp4"),
            None
        );
        // ファイル名に `%` を含む通知行を進捗と誤認しない。
        assert_eq!(
            yt_dlp_download_percent("[download] Destination: 100% Orange Juice OP.mp4"),
            None
        );
    }
}
