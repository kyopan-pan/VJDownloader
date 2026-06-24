use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use crate::download::{ProcessTracker, js_runtime_arg};
use crate::fs_utils::is_executable;
use crate::paths::{bin_dir, ffmpeg_path, yt_dlp_path};

// ウィンドウ内ミニプレビューのデコード解像度（固定サイズの生RGBAフレーム）。
pub const PREVIEW_WIDTH: usize = 480;
pub const PREVIEW_HEIGHT: usize = 270;
const FRAME_BYTES: usize = PREVIEW_WIDTH * PREVIEW_HEIGHT * 4;

// ストリーム再生中に UI へ通知するイベント。
pub enum StreamEvent {
    Log(String),
    Finished(Result<(), String>),
}

// プレビュー表示用にデコードした 1 フレーム（RGBA）。
pub struct StreamFrame {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

// yt-dlp → ffmpeg のパイプラインを起動し、終了まで監視する。
//
// ffmpeg は映像を生RGBAフレームへ変換して標準出力へ流し（プレビュー描画用）、
// 音声は macOS の AudioToolbox 出力デバイスへ実時間再生する。音声側が実時間で
// 律速するため、映像フレームの供給ペースも再生速度に揃う。
pub fn run_stream(
    url: String,
    cookie_args: Vec<String>,
    tx: mpsc::Sender<StreamEvent>,
    frame_tx: mpsc::Sender<StreamFrame>,
    cancel_flag: Arc<AtomicBool>,
    tracker: ProcessTracker,
) {
    let result = run_stream_inner(url, cookie_args, &tx, frame_tx, &cancel_flag, &tracker);
    let _ = tx.send(StreamEvent::Finished(result));
}

fn run_stream_inner(
    url: String,
    cookie_args: Vec<String>,
    tx: &mpsc::Sender<StreamEvent>,
    frame_tx: mpsc::Sender<StreamFrame>,
    cancel_flag: &Arc<AtomicBool>,
    tracker: &ProcessTracker,
) -> Result<(), String> {
    let yt_dlp = yt_dlp_path();
    if !yt_dlp.exists() || !is_executable(&yt_dlp) {
        return Err("yt-dlpが見つかりません。".to_string());
    }
    let ffmpeg = ffmpeg_path();
    if !ffmpeg.exists() {
        return Err("ffmpegが見つかりません。".to_string());
    }

    if cancel_flag.load(Ordering::Relaxed) {
        return Ok(());
    }

    // yt-dlp: 映像+音声を matroska へ多重化して標準出力へ送る。
    let args = build_yt_dlp_args(&url, &cookie_args, &ffmpeg.to_string_lossy());
    let mut yt_dlp_cmd = Command::new(&yt_dlp);
    yt_dlp_cmd
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_bin_path(&mut yt_dlp_cmd);

    let mut yt_dlp_child = yt_dlp_cmd
        .spawn()
        .map_err(|err| format!("yt-dlpの起動に失敗しました: {err}"))?;
    tracker.register(&yt_dlp_child);

    let yt_dlp_stdout = yt_dlp_child
        .stdout
        .take()
        .ok_or_else(|| "yt-dlp出力の取得に失敗しました。".to_string())?;
    spawn_log_thread(yt_dlp_child.stderr.take(), tx.clone());

    // ffmpeg: 映像を生RGBA(pipe:1)へ、音声を AudioToolbox へ同時出力する。
    let mut ffmpeg_child = match build_ffmpeg_command(&ffmpeg, yt_dlp_stdout).spawn() {
        Ok(child) => child,
        Err(err) => {
            tracker.terminate_all();
            let _ = yt_dlp_child.wait();
            return Err(format!("ffmpegの起動に失敗しました: {err}"));
        }
    };
    tracker.register(&ffmpeg_child);
    spawn_log_thread(ffmpeg_child.stderr.take(), tx.clone());

    let _ = tx.send(StreamEvent::Log("ストリーム再生を開始しました。".to_string()));

    // ffmpeg の生RGBA出力を 1 フレームずつ読み取り、プレビューへ送る。
    if let Some(stdout) = ffmpeg_child.stdout.take() {
        read_frames(stdout, &frame_tx);
    }

    // 出力が尽きた（再生終了/停止）後にプロセスを片付ける。
    tracker.terminate_all();
    let _ = ffmpeg_child.wait();
    let _ = yt_dlp_child.wait();

    Ok(())
}

// ffmpeg の標準出力から固定サイズのRGBAフレームを読み出して送出する。
fn read_frames<R: Read>(reader: R, frame_tx: &mpsc::Sender<StreamFrame>) {
    let mut reader = BufReader::new(reader);
    let mut buf = vec![0u8; FRAME_BYTES];
    loop {
        match reader.read_exact(&mut buf) {
            Ok(()) => {
                let frame = StreamFrame {
                    size: [PREVIEW_WIDTH, PREVIEW_HEIGHT],
                    rgba: buf.clone(),
                };
                if frame_tx.send(frame).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

// プレビュー用 ffmpeg コマンドを組み立てる。
fn build_ffmpeg_command(ffmpeg: &std::path::Path, stdin: std::process::ChildStdout) -> Command {
    let video_filter = format!(
        "fps=30,scale={w}:{h}:force_original_aspect_ratio=decrease,\
         pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black",
        w = PREVIEW_WIDTH,
        h = PREVIEW_HEIGHT,
    );

    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg("pipe:0")
        // 映像: 生RGBAフレームを標準出力へ。
        .arg("-map")
        .arg("0:v?")
        .arg("-vf")
        .arg(&video_filter)
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-f")
        .arg("rawvideo")
        .arg("pipe:1")
        // 音声: AudioToolbox の既定デバイスへ実時間再生。
        .arg("-map")
        .arg("0:a?")
        .arg("-f")
        .arg("audiotoolbox")
        .arg("-audio_device_index")
        .arg("-1")
        .arg("vjstream_audio")
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

// ストリーム再生用の yt-dlp 引数を組み立てる。
fn build_yt_dlp_args(url: &str, cookie_args: &[String], ffmpeg_path: &str) -> Vec<String> {
    let js_runtime = js_runtime_arg();
    let mut args = vec!["--no-playlist".to_string()];
    args.extend(cookie_args.iter().cloned());
    args.extend([
        "--extractor-args".to_string(),
        "youtube:player_client=web".to_string(),
        "--extractor-args".to_string(),
        "youtube:skip=translated_subs".to_string(),
        "-f".to_string(),
        "bv*[height<=480]+ba/b[height<=480]/b".to_string(),
        "--merge-output-format".to_string(),
        "mkv".to_string(),
        "--ffmpeg-location".to_string(),
        ffmpeg_path.to_string(),
        "--js-runtimes".to_string(),
        js_runtime,
        "-o".to_string(),
        "-".to_string(),
        url.to_string(),
    ]);
    args
}

// yt-dlp が同梱 ffmpeg/deno を見つけられるよう bin を PATH 先頭へ追加する。
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

// 子プロセスの出力を 1 行ずつログイベントへ変換する。
fn spawn_log_thread<R: Read + Send + 'static>(reader: Option<R>, tx: mpsc::Sender<StreamEvent>) {
    let Some(reader) = reader else {
        return;
    };
    thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines() {
            let Ok(line) = line else {
                break;
            };
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let _ = tx.send(StreamEvent::Log(trimmed.to_string()));
            }
        }
    });
}
