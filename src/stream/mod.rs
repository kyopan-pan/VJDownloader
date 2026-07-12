pub mod ui;

// Syphon 出力（マスターを VDMX 等へ共有）。公式 Syphon.framework をリンクする
// `syphon` フィーチャー有効時のみ有効化される。
#[cfg(feature = "syphon")]
pub mod syphon;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use url::Url;

use crate::download::{ProcessTracker, js_runtime_arg};
use crate::fs_utils::is_executable;
use crate::paths::{bin_dir, ffmpeg_path, yt_dlp_path};

// デコード解像度（固定サイズの生RGBAフレーム）。
// Syphon 出力時はマスターを高解像度で配信するため 1280x720、通常は軽量な 480x270。
#[cfg(feature = "syphon")]
pub const PREVIEW_WIDTH: usize = 1280;
#[cfg(feature = "syphon")]
pub const PREVIEW_HEIGHT: usize = 720;
#[cfg(not(feature = "syphon"))]
pub const PREVIEW_WIDTH: usize = 480;
#[cfg(not(feature = "syphon"))]
pub const PREVIEW_HEIGHT: usize = 270;

// プレビューは固定フレームレート(CFR)でデコードし、フレーム番号から提示時刻(PTS)を算出する。
pub const PREVIEW_FPS: f64 = 30.0;
const FRAME_BYTES: usize = PREVIEW_WIDTH * PREVIEW_HEIGHT * 4;
const YOUTUBE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36";
const YOUTUBE_INPUT_HEADERS: &str = "Accept: text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8\r\n\
Accept-Language: en-us,en;q=0.5\r\n\
Sec-Fetch-Mode: navigate\r\n";

#[cfg(feature = "syphon")]
const FORMAT_SELECTOR: &str = "bv*[height<=720]+ba/b[height<=720]/b";
#[cfg(not(feature = "syphon"))]
const FORMAT_SELECTOR: &str = "bv*[height<=480]+ba/b[height<=480]/b";

// ストリーム再生中に UI へ通知するイベント。run_id で再生世代を識別する。
pub enum StreamEvent {
    Resolved {
        run_id: u64,
        duration: Option<f64>,
        urls: Vec<String>,
    },
    Position {
        run_id: u64,
        secs: f64,
    },
    Finished {
        run_id: u64,
        result: Result<(), String>,
    },
}

// プレビュー表示用にデコードした 1 フレーム（RGBA）。pts は再生開始からの絶対秒。
pub struct StreamFrame {
    pub run_id: u64,
    pub pts: f64,
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

// クリップボードURLを解決して再生を開始する（初回再生用）。
//
// yt-dlp で総再生時間と直リンク（映像/音声）を取得し、それを cache 用に通知してから
// ffmpeg を起動する。直リンクを使うため、以後のシークは ffmpeg の再起動だけで済む。
pub fn resolve_and_run(
    url: String,
    cookie_args: Vec<String>,
    start_offset: f64,
    run_id: u64,
    tx: mpsc::Sender<StreamEvent>,
    frame_tx: mpsc::Sender<StreamFrame>,
    cancel_flag: Arc<AtomicBool>,
    tracker: ProcessTracker,
) {
    let (duration, urls) = match resolve_media(&url, &cookie_args) {
        Ok(resolved) => resolved,
        Err(err) => {
            let _ = tx.send(StreamEvent::Finished {
                run_id,
                result: Err(err),
            });
            return;
        }
    };

    let _ = tx.send(StreamEvent::Resolved {
        run_id,
        duration,
        urls: urls.clone(),
    });

    let result = run_ffmpeg(&urls, start_offset, run_id, &tx, &frame_tx, &cancel_flag, &tracker);
    let _ = tx.send(StreamEvent::Finished { run_id, result });
}

// 解決済みの直リンクから指定位置で再生を開始する（シーク用）。
pub fn run_from_urls(
    urls: Vec<String>,
    start_offset: f64,
    run_id: u64,
    tx: mpsc::Sender<StreamEvent>,
    frame_tx: mpsc::Sender<StreamFrame>,
    cancel_flag: Arc<AtomicBool>,
    tracker: ProcessTracker,
) {
    let result = run_ffmpeg(&urls, start_offset, run_id, &tx, &frame_tx, &cancel_flag, &tracker);
    let _ = tx.send(StreamEvent::Finished { run_id, result });
}

// yt-dlp で総再生時間と直リンクを取得する。
fn resolve_media(url: &str, cookie_args: &[String]) -> Result<(Option<f64>, Vec<String>), String> {
    let yt_dlp = yt_dlp_path();
    if !yt_dlp.exists() || !is_executable(&yt_dlp) {
        return Err("yt-dlpが見つかりません。".to_string());
    }

    println!("[stream] yt-dlp resolve start: {url}");
    let mut cmd = Command::new(&yt_dlp);
    cmd.arg("--no-playlist");
    cmd.args(cookie_args);
    cmd.args([
        "--extractor-args",
        "youtube:player_client=web",
        "--extractor-args",
        "youtube:skip=translated_subs",
    ]);
    cmd.arg("--js-runtimes").arg(js_runtime_arg());
    cmd.arg("-f").arg(FORMAT_SELECTOR);
    cmd.arg("--print").arg("DURATION:%(duration)s");
    cmd.arg("-g");
    cmd.arg(url);
    apply_bin_path(&mut cmd);

    let output = cmd
        .output()
        .map_err(|err| format!("yt-dlpの起動に失敗しました: {err}"))?;
    println!("[stream] yt-dlp exited: {}", output.status);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !stderr.is_empty() {
            println!("[stream] yt-dlp stderr:\n{}", sanitize_log_text(&stderr));
        }
        let detail = if stderr.is_empty() {
            output.status.to_string()
        } else {
            sanitize_log_text(&stderr)
        };
        return Err(format!("URL解決に失敗しました: {detail}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut duration = None;
    let mut urls = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("DURATION:") {
            duration = parse_duration(rest);
        } else if line.starts_with("http://") || line.starts_with("https://") {
            urls.push(line.to_string());
        }
    }

    if urls.is_empty() {
        return Err("再生用URLを取得できませんでした。".to_string());
    }
    println!(
        "[stream] yt-dlp resolved: duration={:?}, urls={}",
        duration,
        urls.len()
    );
    for (index, url) in urls.iter().enumerate() {
        println!("[stream] media url {index}: {}", summarize_media_url(url));
    }
    Ok((duration, urls))
}

fn parse_duration(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "NA" || trimmed == "None" {
        return None;
    }
    trimmed.parse::<f64>().ok().filter(|value| *value > 0.0)
}

// ffmpeg を start_offset 秒から起動し、終了まで映像フレームと再生位置を流す。
fn run_ffmpeg(
    urls: &[String],
    start_offset: f64,
    run_id: u64,
    tx: &mpsc::Sender<StreamEvent>,
    frame_tx: &mpsc::Sender<StreamFrame>,
    cancel_flag: &Arc<AtomicBool>,
    tracker: &ProcessTracker,
) -> Result<(), String> {
    let ffmpeg = ffmpeg_path();
    if !ffmpeg.exists() {
        return Err("ffmpegが見つかりません。".to_string());
    }
    if cancel_flag.load(Ordering::Relaxed) {
        return Ok(());
    }

    println!(
        "[stream] ffmpeg start: inputs={}, offset={start_offset:.3}, size={}x{}, fps={}",
        urls.len(),
        PREVIEW_WIDTH,
        PREVIEW_HEIGHT,
        PREVIEW_FPS
    );
    let mut child = build_ffmpeg_command(&ffmpeg, urls, start_offset)
        .spawn()
        .map_err(|err| format!("ffmpegの起動に失敗しました: {err}"))?;
    tracker.register(&child);

    let (stderr_tx, stderr_rx) = mpsc::channel();
    let stderr_handle = if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        Some(thread::spawn(move || {
            read_progress(stderr, tx, stderr_tx, run_id, start_offset)
        }))
    } else {
        None
    };

    let (frame_count, receiver_closed) = match child.stdout.take() {
        Some(stdout) => read_frames(stdout, frame_tx, run_id, start_offset),
        None => (0, false),
    };

    if receiver_closed || cancel_flag.load(Ordering::Relaxed) {
        tracker.terminate_all();
        let _ = child.wait();
        println!(
            "[stream] ffmpeg stopped by receiver/cancel: frames={frame_count}, receiver_closed={receiver_closed}"
        );
        return Ok(());
    }

    let status = child
        .wait()
        .map_err(|err| format!("ffmpegの終了待ちに失敗しました: {err}"))?;
    if let Some(handle) = stderr_handle {
        let _ = handle.join();
    }
    let stderr_tail = collect_stderr_tail(stderr_rx);
    println!("[stream] ffmpeg exited: {status}, frames={frame_count}");
    if !stderr_tail.is_empty() {
        println!("[stream] ffmpeg stderr tail:\n{stderr_tail}");
    }
    if !status.success() {
        let detail = if stderr_tail.is_empty() {
            status.to_string()
        } else {
            format!("{status}: {stderr_tail}")
        };
        return Err(format!("ffmpegが異常終了しました: {detail}"));
    }
    if frame_count == 0 {
        let detail = if stderr_tail.is_empty() {
            "詳細なし".to_string()
        } else {
            stderr_tail
        };
        return Err(format!("ffmpegから映像フレームを取得できませんでした: {detail}"));
    }
    Ok(())
}

// プレビュー用 ffmpeg コマンドを組み立てる。
fn build_ffmpeg_command(ffmpeg: &Path, urls: &[String], start_offset: f64) -> Command {
    let video_filter = format!(
        "fps=30,scale={w}:{h}:force_original_aspect_ratio=decrease,\
         pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black",
        w = PREVIEW_WIDTH,
        h = PREVIEW_HEIGHT,
    );
    let offset = format!("{start_offset:.3}");

    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-stats")
        .arg("-stats_period")
        .arg("0.1");

    // 各入力URLにHTTP再接続オプションとシーク位置を付与する。
    for url in urls {
        cmd.arg("-reconnect")
            .arg("1")
            .arg("-reconnect_streamed")
            .arg("1")
            .arg("-reconnect_delay_max")
            .arg("5")
            .arg("-user_agent")
            .arg(YOUTUBE_USER_AGENT)
            .arg("-referer")
            .arg("https://www.youtube.com/")
            .arg("-headers")
            .arg(YOUTUBE_INPUT_HEADERS)
            .arg("-ss")
            .arg(&offset)
            .arg("-i")
            .arg(url);
    }

    // 音声は2入力構成なら2番目（index 1）、単一入力なら index 0 から取る。
    let audio_input = if urls.len() >= 2 { "1:a?" } else { "0:a?" };

    cmd.arg("-map")
        .arg("0:v?")
        .arg("-vf")
        .arg(&video_filter)
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-f")
        .arg("rawvideo")
        .arg("pipe:1")
        .arg("-map")
        .arg(audio_input)
        .arg("-f")
        .arg("audiotoolbox")
        .arg("-audio_device_index")
        .arg("-1")
        .arg("vjstream_audio")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

// ffmpeg の標準出力から固定サイズのRGBAフレームを読み出し、PTSを付けて送出する。
fn read_frames<R: Read>(
    reader: R,
    frame_tx: &mpsc::Sender<StreamFrame>,
    run_id: u64,
    start_offset: f64,
) -> (u64, bool) {
    let mut reader = std::io::BufReader::new(reader);
    let mut buf = vec![0u8; FRAME_BYTES];
    let mut index: u64 = 0;
    let mut receiver_closed = false;
    loop {
        match reader.read_exact(&mut buf) {
            Ok(()) => {
                let frame = StreamFrame {
                    run_id,
                    pts: start_offset + index as f64 / PREVIEW_FPS,
                    size: [PREVIEW_WIDTH, PREVIEW_HEIGHT],
                    rgba: buf.clone(),
                };
                if frame_tx.send(frame).is_err() {
                    receiver_closed = true;
                    break;
                }
                index += 1;
            }
            Err(_) => break,
        }
    }
    (index, receiver_closed)
}

// ffmpeg の stderr を解析し、再生位置(time=)を Position、その他をログへ送る。
fn read_progress<R: Read>(
    reader: R,
    tx: mpsc::Sender<StreamEvent>,
    stderr_tx: mpsc::Sender<String>,
    run_id: u64,
    start_offset: f64,
) {
    let mut reader = std::io::BufReader::new(reader);
    let mut buf = [0u8; 4096];
    let mut segment = Vec::new();
    loop {
        let read = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        for &byte in &buf[..read] {
            // ffmpeg の進捗は \r 区切りのため改行・復帰の双方で分割する。
            if byte == b'\n' || byte == b'\r' {
                flush_progress_segment(&mut segment, &tx, &stderr_tx, run_id, start_offset);
            } else {
                segment.push(byte);
            }
        }
    }
    flush_progress_segment(&mut segment, &tx, &stderr_tx, run_id, start_offset);
}

fn flush_progress_segment(
    segment: &mut Vec<u8>,
    tx: &mpsc::Sender<StreamEvent>,
    stderr_tx: &mpsc::Sender<String>,
    run_id: u64,
    start_offset: f64,
) {
    if segment.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(segment).trim().to_string();
    segment.clear();
    if text.is_empty() {
        return;
    }

    if let Some(secs) = parse_ffmpeg_time(&text) {
        let _ = tx.send(StreamEvent::Position {
            run_id,
            secs: start_offset + secs,
        });
    } else {
        let sanitized = sanitize_log_text(&text);
        println!("[stream] ffmpeg stderr: {sanitized}");
        let _ = stderr_tx.send(sanitized);
    }
}

fn collect_stderr_tail(rx: mpsc::Receiver<String>) -> String {
    let mut lines = Vec::new();
    while let Ok(line) = rx.try_recv() {
        lines.push(line);
    }
    let joined = lines.join("\n");
    const MAX_LEN: usize = 1200;
    if joined.len() <= MAX_LEN {
        joined
    } else {
        let mut tail = joined
            .chars()
            .rev()
            .take(MAX_LEN)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        tail.insert_str(0, "...");
        tail
    }
}

fn summarize_media_url(raw: &str) -> String {
    let Ok(url) = Url::parse(raw) else {
        return "[media-url]".to_string();
    };
    let host = url.host_str().unwrap_or("unknown-host");
    let mut parts = Vec::new();
    for key in ["itag", "mime", "c", "dur", "clen"] {
        if let Some((_, value)) = url.query_pairs().find(|(name, _)| name == key) {
            parts.push(format!("{key}={value}"));
        }
    }
    if parts.is_empty() {
        host.to_string()
    } else {
        format!("{host}?{}", parts.join("&"))
    }
}

fn sanitize_log_text(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            if token.starts_with("http://") || token.starts_with("https://") {
                summarize_media_url(token)
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ffmpeg stats 行から `time=HH:MM:SS.xx` を秒に変換する。
fn parse_ffmpeg_time(line: &str) -> Option<f64> {
    let idx = line.find("time=")?;
    let token = line[idx + 5..].split_whitespace().next()?;
    let mut parts = token.split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

// yt-dlp が同梱 deno を見つけられるよう bin を PATH 先頭へ追加する。
fn apply_bin_path(command: &mut Command) {
    let mut paths = Vec::new();
    let bin = bin_dir();
    if bin.exists() {
        paths.push(bin.into_os_string());
    }
    if let Some(current) = std::env::var_os("PATH") {
        paths.push(current);
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        command.env("PATH", joined);
    }
}
