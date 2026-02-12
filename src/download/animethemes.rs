use serde_json::Value;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use url::Url;

use crate::paths::ffprobe_path;

use super::process::{run_pipe_to_ffmpeg_or_cancel, spawn_stream_thread, terminate_child_process};
use super::{CANCELLED_ERROR, DownloadEvent, ProcessTracker, ProgressContext, ProgressUpdate};

const ANIMETHEMES_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const ANIMETHEMES_API_ENDPOINT: &str = "https://api.animethemes.moe";
const ANIMETHEMES_HTML_RANGE: &str = "0-262143";

// AnimeThemes URL の場合に、直リンク優先で MP4 を生成する専用パイプラインを実行する。
pub(super) fn run_animethemes_pipeline(
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
    ensure_apple_silicon_gpu_encoder(ffmpeg)?;
    let output_path = build_animethemes_output_path(url, output_dir);

    let direct_url = fetch_animethemes_direct_webm(url, tx)?;
    match direct_url {
        Some(webm_url) => {
            let _ = tx.send(DownloadEvent::Log(format!(
                "AnimeThemes直リンクを取得しました: {webm_url}"
            )));
            let temp_webm_path = build_animethemes_temp_webm_path(&output_path);
            download_animethemes_webm_with_progress(
                &webm_url,
                &temp_webm_path,
                tx,
                progress,
                tracker,
                cancel_flag,
            )?;
            let convert_result = convert_animethemes_webm_to_mp4_with_gpu(
                ffmpeg,
                &temp_webm_path,
                &output_path,
                tx,
                progress,
                tracker,
                cancel_flag,
            );
            let _ = fs::remove_file(&temp_webm_path);
            convert_result?;
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
            run_pipe_to_ffmpeg_or_cancel(
                cmd,
                ffmpeg,
                &output_path,
                tx,
                progress,
                "webm",
                tracker,
                cancel_flag,
            )?;
        }
    }

    Ok(())
}

// 変換前に保存する一時 WebM ファイル名を作る。
fn build_animethemes_temp_webm_path(output_path: &Path) -> PathBuf {
    let mut temp = output_path.to_path_buf();
    temp.set_extension("webm.part");
    temp
}

// curl で WebM を取得し、進捗イベントを発行しながら一時ファイルへ保存する。
fn download_animethemes_webm_with_progress(
    webm_url: &str,
    temp_webm_path: &Path,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    tracker: &ProcessTracker,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(), String> {
    let _ = tx.send(DownloadEvent::Log(
        "動画ダウンロードを開始します。".to_string(),
    ));
    let total_bytes = fetch_content_length(webm_url);
    if let Some(total) = total_bytes {
        let _ = tx.send(DownloadEvent::Log(format!(
            "動画サイズを確認しました: {:.1}MB",
            total as f64 / (1024.0 * 1024.0)
        )));
    } else {
        let _ = tx.send(DownloadEvent::Log(
            "動画サイズを取得できなかったため、MBベースで進捗ログを表示します。".to_string(),
        ));
    }

    let mut curl_cmd = Command::new("curl");
    curl_cmd
        .arg("-sS")
        .arg("-L")
        .arg("-m")
        .arg("120")
        .arg("--fail")
        .arg("-o")
        .arg("-")
        .arg("-A")
        .arg(ANIMETHEMES_USER_AGENT)
        .arg(webm_url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut curl_child = curl_cmd
        .spawn()
        .map_err(|err| format!("curl起動に失敗しました: {err}"))?;
    tracker.register(&curl_child);
    spawn_stream_thread(curl_child.stderr.take(), tx, progress);

    let mut curl_stdout = match curl_child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_child_process(&mut curl_child);
            return Err("curl出力の取得に失敗しました。".to_string());
        }
    };
    let mut output_file = match fs::File::create(temp_webm_path) {
        Ok(file) => file,
        Err(err) => {
            terminate_child_process(&mut curl_child);
            return Err(format!("一時ファイルの作成に失敗しました: {err}"));
        }
    };

    let mut downloaded: u64 = 0;
    let mut last_log_bucket: i64 = -1;
    let mut last_bytes_log: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            terminate_child_process(&mut curl_child);
            let _ = fs::remove_file(temp_webm_path);
            return Err(CANCELLED_ERROR.to_string());
        }

        let read = match curl_stdout.read(&mut buf) {
            Ok(read) => read,
            Err(err) => {
                terminate_child_process(&mut curl_child);
                let _ = fs::remove_file(temp_webm_path);
                return Err(format!("動画ストリームの読み取りに失敗しました: {err}"));
            }
        };
        if read == 0 {
            break;
        }
        if let Err(err) = output_file.write_all(&buf[..read]) {
            terminate_child_process(&mut curl_child);
            let _ = fs::remove_file(temp_webm_path);
            return Err(format!("一時ファイルへの書き込みに失敗しました: {err}"));
        }

        downloaded += read as u64;
        if let Some(total) = total_bytes {
            if total > 0 {
                let percent = (downloaded as f64 * 100.0 / total as f64).clamp(0.0, 100.0) as f32;
                let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::downloading(
                    percent,
                    &progress.elapsed(),
                )));
                let bucket = (percent / 5.0).floor() as i64;
                if bucket > last_log_bucket {
                    last_log_bucket = bucket;
                    let _ = tx.send(DownloadEvent::Log(format!(
                        "ダウンロード進捗: {:.1}%",
                        percent
                    )));
                }
            }
        } else if downloaded >= last_bytes_log.saturating_add(10 * 1024 * 1024) {
            last_bytes_log = downloaded;
            let _ = tx.send(DownloadEvent::Log(format!(
                "ダウンロード進捗: {:.1}MB",
                downloaded as f64 / (1024.0 * 1024.0)
            )));
        }
    }

    if let Err(err) = output_file.flush() {
        terminate_child_process(&mut curl_child);
        let _ = fs::remove_file(temp_webm_path);
        return Err(format!("一時ファイルの保存に失敗しました: {err}"));
    }

    let curl_status = curl_child
        .wait()
        .map_err(|err| format!("curlの終了待ちに失敗しました: {err}"))?;

    if cancel_flag.load(Ordering::Relaxed) {
        let _ = fs::remove_file(temp_webm_path);
        return Err(CANCELLED_ERROR.to_string());
    }
    if !curl_status.success() {
        let _ = fs::remove_file(temp_webm_path);
        return Err(format!("curlが異常終了しました: {curl_status}"));
    }

    let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::downloading(
        100.0,
        &progress.elapsed(),
    )));
    let _ = tx.send(DownloadEvent::Log("ダウンロード進捗: 100.0%".to_string()));
    let _ = tx.send(DownloadEvent::Log(
        "動画ダウンロードが完了しました。".to_string(),
    ));
    Ok(())
}

// WebM を Apple Silicon GPU エンコーダで MP4 へ変換し、進捗を UI へ送る。
fn convert_animethemes_webm_to_mp4_with_gpu(
    ffmpeg: &Path,
    input_webm_path: &Path,
    output_path: &Path,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    tracker: &ProcessTracker,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(), String> {
    if cancel_flag.load(Ordering::Relaxed) {
        return Err(CANCELLED_ERROR.to_string());
    }
    progress.set_post_processing();
    let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::post_processing(
        &progress.elapsed(),
    )));
    let _ = tx.send(DownloadEvent::Log(
        "ffmpeg(GPU: h264_videotoolbox)で変換を開始します。".to_string(),
    ));
    let conversion_total_seconds = probe_media_duration_seconds(input_webm_path);
    if conversion_total_seconds.is_none() {
        let _ = tx.send(DownloadEvent::Log(
            "ffprobeで長さ取得に失敗したため、変換進捗バーは概算表示になります。".to_string(),
        ));
    }

    let mut ffmpeg_cmd = Command::new(ffmpeg);
    ffmpeg_cmd
        .arg("-stats")
        .arg("-analyzeduration")
        .arg("100M")
        .arg("-probesize")
        .arg("100M")
        .arg("-i")
        .arg(input_webm_path.to_string_lossy().to_string())
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut ffmpeg_child = ffmpeg_cmd
        .spawn()
        .map_err(|err| format!("ffmpeg起動に失敗しました: {err}"))?;
    tracker.register(&ffmpeg_child);
    spawn_stream_thread(ffmpeg_child.stdout.take(), tx, progress);
    spawn_ffmpeg_conversion_thread(
        ffmpeg_child.stderr.take(),
        tx,
        progress,
        conversion_total_seconds,
    );

    let ffmpeg_status = ffmpeg_child
        .wait()
        .map_err(|err| format!("ffmpegの終了待ちに失敗しました: {err}"))?;
    if cancel_flag.load(Ordering::Relaxed) {
        return Err(CANCELLED_ERROR.to_string());
    }
    if !ffmpeg_status.success() {
        return Err(format!("ffmpegが異常終了しました: {ffmpeg_status}"));
    }
    let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::converting(
        100.0,
        &progress.elapsed(),
    )));
    let _ = tx.send(DownloadEvent::Log("ffmpeg変換が完了しました。".to_string()));
    Ok(())
}

// ffprobe でメディア長を秒単位で取得する。
fn probe_media_duration_seconds(path: &Path) -> Option<f64> {
    let ffprobe = ffprobe_path();
    if !ffprobe.exists() {
        return None;
    }
    let output = Command::new(ffprobe)
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(path.to_string_lossy().to_string())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let duration = text.trim().parse::<f64>().ok()?;
    if duration.is_finite() && duration > 0.0 {
        Some(duration)
    } else {
        None
    }
}

// ffmpeg の stderr を解析して変換進捗を推定するスレッドを起動する。
fn spawn_ffmpeg_conversion_thread<R: Read + Send + 'static>(
    reader: Option<R>,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    total_seconds: Option<f64>,
) {
    if let Some(reader) = reader {
        let tx_clone = tx.clone();
        let progress_clone = progress.clone();
        thread::spawn(move || {
            stream_ffmpeg_conversion_lines(reader, tx_clone, progress_clone, total_seconds)
        });
    }
}

// ffmpeg ログを CR/LF 単位で区切り、進捗行を解析する。
fn stream_ffmpeg_conversion_lines<R: Read + Send + 'static>(
    reader: R,
    tx: mpsc::Sender<DownloadEvent>,
    progress: Arc<ProgressContext>,
    total_seconds: Option<f64>,
) {
    let mut buffered = BufReader::new(reader);
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    let mut last_percent: f32 = -1.0;
    loop {
        let read = match buffered.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        for &byte in &buf[..read] {
            if byte == b'\n' || byte == b'\r' {
                if !line.is_empty() {
                    let text = String::from_utf8_lossy(&line).to_string();
                    handle_ffmpeg_conversion_line(
                        text,
                        &tx,
                        &progress,
                        total_seconds,
                        &mut last_percent,
                    );
                    line.clear();
                }
            } else {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        let text = String::from_utf8_lossy(&line).to_string();
        handle_ffmpeg_conversion_line(text, &tx, &progress, total_seconds, &mut last_percent);
    }
}

// 1 行ログから "time=..." を取り出し、全体時間に対する割合を更新する。
fn handle_ffmpeg_conversion_line(
    line: String,
    tx: &mpsc::Sender<DownloadEvent>,
    progress: &Arc<ProgressContext>,
    total_seconds: Option<f64>,
    last_percent: &mut f32,
) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    if let Some(total) = total_seconds {
        if total > 0.0 {
            if let Some(current) = parse_ffmpeg_time_seconds(trimmed) {
                let percent = ((current / total) * 100.0).clamp(0.0, 100.0) as f32;
                if percent >= *last_percent + 0.2 || percent >= 99.9 {
                    *last_percent = percent;
                    let _ = tx.send(DownloadEvent::Progress(ProgressUpdate::converting(
                        percent,
                        &progress.elapsed(),
                    )));
                }
            }
        }
    }

    let _ = tx.send(DownloadEvent::Log(trimmed.to_string()));
}

fn parse_ffmpeg_time_seconds(line: &str) -> Option<f64> {
    let idx = line.find("time=")?;
    let after = &line[idx + "time=".len()..];
    let token = after.split_whitespace().next()?;
    parse_hhmmss_to_seconds(token)
}

fn parse_hhmmss_to_seconds(value: &str) -> Option<f64> {
    let mut parts = value.split(':');
    let hours = parts.next()?.trim().parse::<f64>().ok()?;
    let minutes = parts.next()?.trim().parse::<f64>().ok()?;
    let seconds = parts.next()?.trim().parse::<f64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

// HEAD/Range の順で Content-Length を取得し、進捗計算に使う。
fn fetch_content_length(url: &str) -> Option<u64> {
    let head_output = Command::new("curl")
        .arg("-sIL")
        .arg("-m")
        .arg("8")
        .arg("-A")
        .arg(ANIMETHEMES_USER_AGENT)
        .arg(url)
        .output()
        .ok()?;
    if head_output.status.success() {
        let headers = String::from_utf8_lossy(&head_output.stdout);
        if let Some(len) = parse_content_length_from_headers(&headers) {
            return Some(len);
        }
    }

    let range_output = Command::new("curl")
        .arg("-sSL")
        .arg("-m")
        .arg("10")
        .arg("-A")
        .arg(ANIMETHEMES_USER_AGENT)
        .arg("-r")
        .arg("0-0")
        .arg("-D")
        .arg("-")
        .arg("-o")
        .arg("/dev/null")
        .arg(url)
        .output()
        .ok()?;
    if !range_output.status.success() {
        return None;
    }
    let headers = String::from_utf8_lossy(&range_output.stdout);
    parse_content_range_total(&headers).or_else(|| parse_content_length_from_headers(&headers))
}

fn parse_content_length_from_headers(headers: &str) -> Option<u64> {
    let mut result = None;
    for line in headers.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            if let Ok(len) = value.trim().parse::<u64>() {
                result = Some(len);
            }
        }
    }
    result
}

fn parse_content_range_total(headers: &str) -> Option<u64> {
    let mut result = None;
    for line in headers.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-range:") {
            if let Some((_, total_part)) = value.rsplit_once('/') {
                if let Ok(total) = total_part.trim().parse::<u64>() {
                    result = Some(total);
                }
            }
        }
    }
    result
}

// Apple Silicon + h264_videotoolbox 前提を満たしているかを検証する。
fn ensure_apple_silicon_gpu_encoder(ffmpeg: &Path) -> Result<(), String> {
    if std::env::consts::ARCH != "aarch64" {
        return Err(
            "Apple Silicon環境のみ対応です。h264_videotoolbox(GPU)が必須です。".to_string(),
        );
    }
    let output = Command::new(ffmpeg)
        .arg("-hide_banner")
        .arg("-encoders")
        .output()
        .map_err(|err| format!("ffmpegエンコーダ確認に失敗しました: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffmpegエンコーダ確認に失敗しました: {}",
            output.status
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let joined = format!("{stdout}\n{stderr}");
    if !joined.contains("h264_videotoolbox") {
        return Err(
            "ffmpegにh264_videotoolboxがありません。Apple Silicon GPU変換を継続できません。"
                .to_string(),
        );
    }
    Ok(())
}

// API 取得を優先し、失敗時は HTML 解析で直リンクを探す。
fn fetch_animethemes_direct_webm(
    url: &str,
    tx: &mpsc::Sender<DownloadEvent>,
) -> Result<Option<String>, String> {
    if let Some(webm_url) = fetch_animethemes_webm_via_api(url, tx)? {
        return Ok(Some(webm_url));
    }
    fetch_animethemes_webm_via_html(url, tx)
}

fn fetch_animethemes_webm_via_api(
    page_url: &str,
    tx: &mpsc::Sender<DownloadEvent>,
) -> Result<Option<String>, String> {
    let Some((anime_slug, theme_slug)) = parse_animethemes_page_slugs(page_url) else {
        let _ = tx.send(DownloadEvent::Log(
            "AnimeThemes URL解析に失敗。HTML解析へフォールバックします。".to_string(),
        ));
        return Ok(None);
    };

    let api_urls = vec![
        format!(
            "{ANIMETHEMES_API_ENDPOINT}/anime/{anime_slug}?include=animethemes.animethemeentries.videos"
        ),
        format!(
            "{ANIMETHEMES_API_ENDPOINT}/anime?filter%5Bslug%5D={anime_slug}&include=animethemes.animethemeentries.videos"
        ),
    ];

    for api_url in api_urls {
        let output = Command::new("curl")
            .arg("-sL")
            .arg("-m")
            .arg("8")
            .arg("-A")
            .arg(ANIMETHEMES_USER_AGENT)
            .arg("-H")
            .arg("Accept: application/json")
            .arg(&api_url)
            .output()
            .map_err(|err| format!("AnimeThemes API取得に失敗しました: {err}"))?;

        if !output.status.success() {
            let _ = tx.send(DownloadEvent::Log(format!(
                "AnimeThemes API取得に失敗しました: {} ({api_url})",
                output.status
            )));
            continue;
        }

        let body = String::from_utf8_lossy(&output.stdout);
        match extract_animethemes_webm_from_api_json(&body, &theme_slug) {
            Ok(Some(webm_url)) => return Ok(Some(webm_url)),
            Ok(None) => continue,
            Err(reason) => {
                let _ = tx.send(DownloadEvent::Log(format!(
                    "AnimeThemes APIレスポンス解析に失敗しました: {reason} ({api_url})"
                )));
                continue;
            }
        }
    }

    let _ = tx.send(DownloadEvent::Log(
        "AnimeThemes APIに対象テーマの直リンクがありません。HTML解析へフォールバックします。"
            .to_string(),
    ));
    Ok(None)
}

fn fetch_animethemes_webm_via_html(
    url: &str,
    tx: &mpsc::Sender<DownloadEvent>,
) -> Result<Option<String>, String> {
    let range_output = Command::new("curl")
        .arg("-sL")
        .arg("-m")
        .arg("8")
        .arg("-A")
        .arg(ANIMETHEMES_USER_AGENT)
        .arg("--range")
        .arg(ANIMETHEMES_HTML_RANGE)
        .arg(url)
        .output()
        .map_err(|err| format!("curl起動に失敗しました: {err}"))?;

    if !range_output.status.success() {
        let _ = tx.send(DownloadEvent::Log(format!(
            "AnimeThemesページ取得に失敗しました: {}",
            range_output.status
        )));
        return Ok(None);
    }

    let html = String::from_utf8_lossy(&range_output.stdout);
    if let Some(webm_url) = extract_animethemes_webm(&html) {
        return Ok(Some(webm_url));
    }

    let _ = tx.send(DownloadEvent::Log(
        "AnimeThemes HTML部分取得では直リンクが見つかりません。全文取得で再試行します。"
            .to_string(),
    ));
    let full_output = Command::new("curl")
        .arg("-sL")
        .arg("-m")
        .arg("8")
        .arg("-A")
        .arg(ANIMETHEMES_USER_AGENT)
        .arg(url)
        .output()
        .map_err(|err| format!("curl起動に失敗しました: {err}"))?;

    if !full_output.status.success() {
        let _ = tx.send(DownloadEvent::Log(format!(
            "AnimeThemesページ全文取得に失敗しました: {}",
            full_output.status
        )));
        return Ok(None);
    }

    let full_html = String::from_utf8_lossy(&full_output.stdout);
    Ok(extract_animethemes_webm(&full_html))
}

fn parse_animethemes_page_slugs(url: &str) -> Option<(String, String)> {
    let parsed = Url::parse(url).ok()?;
    let segments = parsed
        .path_segments()?
        .filter(|item| !item.trim().is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 3 || !segments[0].eq_ignore_ascii_case("anime") {
        return None;
    }
    Some((segments[1].to_string(), segments[2].to_string()))
}

fn extract_animethemes_webm_from_api_json(
    json: &str,
    theme_slug: &str,
) -> Result<Option<String>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|err| format!("JSON解析に失敗しました: {err}"))?;
    if let Some(link) = extract_animethemes_webm_from_json_api(&value, theme_slug) {
        return Ok(Some(link));
    }
    if let Some(link) = extract_animethemes_webm_from_nested_payload(&value, theme_slug) {
        return Ok(Some(link));
    }
    Ok(None)
}

#[derive(Clone, Debug)]
struct AnimeThemesVideoCandidate {
    link: String,
    resolution: i64,
    source_priority: i64,
}

fn extract_animethemes_webm_from_json_api(value: &Value, theme_slug: &str) -> Option<String> {
    let included = value.get("included")?.as_array()?;

    let theme_ids = included
        .iter()
        .filter(|item| {
            jsonapi_type(item)
                .map(|kind| kind.eq_ignore_ascii_case("animetheme"))
                .unwrap_or(false)
                && theme_matches_slug(item, theme_slug)
        })
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(|id| id.to_string())
        .collect::<Vec<_>>();

    let mut candidates = Vec::new();
    for theme_id in theme_ids {
        let Some(theme) = find_jsonapi_resource(included, "animetheme", &theme_id) else {
            continue;
        };
        for entry_id in relationship_ids(theme, "animethemeentries") {
            let Some(entry) = find_jsonapi_resource(included, "animethemeentry", &entry_id) else {
                continue;
            };
            for video_id in relationship_ids(entry, "videos") {
                if let Some(video) = find_jsonapi_resource(included, "video", &video_id) {
                    if let Some(candidate) = parse_video_candidate(video) {
                        candidates.push(candidate);
                    }
                }
            }
        }
    }

    pick_best_video_link(candidates)
}

fn extract_animethemes_webm_from_nested_payload(value: &Value, theme_slug: &str) -> Option<String> {
    let mut themes = Vec::new();
    if let Some(anime) = value.get("anime") {
        collect_themes_from_anime_node(anime, &mut themes);
    }
    if let Some(anime) = value.get("data").and_then(|data| data.get("anime")) {
        collect_themes_from_anime_node(anime, &mut themes);
    }
    if let Some(data) = value.get("data") {
        collect_themes_from_anime_node(data, &mut themes);
    }

    let mut candidates = Vec::new();
    for theme in themes {
        if !theme_matches_slug(theme, theme_slug) {
            continue;
        }
        if let Some(entries) = theme.get("animethemeentries").and_then(Value::as_array) {
            for entry in entries {
                if let Some(videos) = entry.get("videos").and_then(Value::as_array) {
                    for video in videos {
                        if let Some(candidate) = parse_video_candidate(video) {
                            candidates.push(candidate);
                        }
                    }
                }
            }
        }
    }

    pick_best_video_link(candidates)
}

fn collect_themes_from_anime_node<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    match node {
        Value::Array(items) => {
            for item in items {
                collect_themes_from_anime_node(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(themes) = map.get("animethemes").and_then(Value::as_array) {
                out.extend(themes.iter());
            }
        }
        _ => {}
    }
}

fn jsonapi_type(resource: &Value) -> Option<&str> {
    resource.get("type").and_then(Value::as_str)
}

fn find_jsonapi_resource<'a>(
    included: &'a [Value],
    type_name: &str,
    id: &str,
) -> Option<&'a Value> {
    included.iter().find(|item| {
        jsonapi_type(item)
            .map(|kind| kind.eq_ignore_ascii_case(type_name))
            .unwrap_or(false)
            && item
                .get("id")
                .and_then(Value::as_str)
                .map(|item_id| item_id == id)
                .unwrap_or(false)
    })
}

fn relationship_ids(resource: &Value, relation: &str) -> Vec<String> {
    let relation_data = resource
        .get("relationships")
        .and_then(|v| v.get(relation))
        .and_then(|v| v.get("data"));

    match relation_data {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("id").and_then(Value::as_str))
            .map(|id| id.to_string())
            .collect(),
        Some(Value::Object(item)) => item
            .get("id")
            .and_then(Value::as_str)
            .map(|id| vec![id.to_string()])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn parse_video_candidate(video: &Value) -> Option<AnimeThemesVideoCandidate> {
    let attributes = video.get("attributes").unwrap_or(video);
    let link = attributes
        .get("link")
        .and_then(Value::as_str)
        .and_then(normalize_animethemes_video_link)?;
    if !is_animethemes_webm_url(&link) {
        return None;
    }

    let resolution = attributes
        .get("resolution")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let source = attributes
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();

    Some(AnimeThemesVideoCandidate {
        link,
        resolution,
        source_priority: source_priority(source),
    })
}

fn source_priority(source: &str) -> i64 {
    match source.to_ascii_uppercase().as_str() {
        "BD" => 3,
        "WEB" => 2,
        "DVD" => 1,
        _ => 0,
    }
}

fn pick_best_video_link(candidates: Vec<AnimeThemesVideoCandidate>) -> Option<String> {
    candidates
        .into_iter()
        .max_by_key(|candidate| (candidate.resolution, candidate.source_priority))
        .map(|candidate| candidate.link)
}

fn theme_matches_slug(theme: &Value, theme_slug: &str) -> bool {
    let attributes = theme.get("attributes").unwrap_or(theme);

    if let Some(slug) = attributes.get("slug").and_then(Value::as_str) {
        if is_matching_theme_identifier(theme_slug, slug) {
            return true;
        }
    }

    let Some(theme_type) = attributes.get("type").and_then(Value::as_str) else {
        return false;
    };
    let Some(sequence) = attributes.get("sequence").and_then(Value::as_i64) else {
        return false;
    };
    let composed = format!("{theme_type}{sequence}");
    is_matching_theme_identifier(theme_slug, &composed)
}

fn is_matching_theme_identifier(target: &str, candidate: &str) -> bool {
    if target.eq_ignore_ascii_case(candidate) {
        return true;
    }
    let target_upper = target.to_ascii_uppercase();
    let candidate_upper = candidate.to_ascii_uppercase();
    if !target_upper.starts_with(&candidate_upper) {
        return false;
    }
    let suffix = &target_upper[candidate_upper.len()..];
    suffix.is_empty()
        || suffix.starts_with('V')
        || suffix.starts_with('-')
        || suffix.starts_with('_')
}

fn is_animethemes_webm_url(url: &str) -> bool {
    let lowered = url.to_ascii_lowercase();
    lowered.starts_with("https://") && lowered.contains(".webm")
}

fn normalize_animethemes_video_link(link: &str) -> Option<String> {
    let mut parsed = Url::parse(link).ok()?;
    if parsed
        .host_str()
        .map(|host| host.eq_ignore_ascii_case("api.animethemes.moe"))
        .unwrap_or(false)
    {
        let _ = parsed.set_host(Some("animethemes.moe"));
    }
    Some(parsed.to_string())
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

// AnimeThemes URL の末尾を使って保存ファイル名を生成する。
fn build_animethemes_output_path(url: &str, output_dir: &Path) -> PathBuf {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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

#[cfg(test)]
mod tests {
    use super::{
        extract_animethemes_webm_from_api_json, parse_content_length_from_headers,
        parse_content_range_total,
    };

    #[test]
    fn extracts_webm_from_json_api_included_response() {
        let json = r#"{
            "data": { "type": "anime", "id": "4776" },
            "included": [
                {
                    "type": "animetheme",
                    "id": "14234",
                    "attributes": { "slug": "OP1" },
                    "relationships": {
                        "animethemeentries": {
                            "data": [{ "type": "animethemeentry", "id": "16647" }]
                        }
                    }
                },
                {
                    "type": "animethemeentry",
                    "id": "16647",
                    "relationships": {
                        "videos": { "data": [{ "type": "video", "id": "19396" }] }
                    }
                },
                {
                    "type": "video",
                    "id": "19396",
                    "attributes": {
                        "link": "https://api.animethemes.moe/video/abc123.webm",
                        "resolution": 1080,
                        "source": "BD"
                    }
                }
            ]
        }"#;

        let actual =
            extract_animethemes_webm_from_api_json(json, "OP1").expect("api json should parse");
        assert_eq!(
            actual.as_deref(),
            Some("https://animethemes.moe/video/abc123.webm")
        );
    }

    #[test]
    fn extracts_best_resolution_from_nested_response() {
        let json = r#"{
            "anime": {
                "animethemes": [
                    {
                        "slug": "OP1",
                        "animethemeentries": [
                            {
                                "videos": [
                                    {
                                        "link": "https://v.animethemes.moe/MeitanteiPrecure-OP1-720.webm",
                                        "resolution": 720,
                                        "source": "WEB"
                                    },
                                    {
                                        "link": "https://v.animethemes.moe/MeitanteiPrecure-OP1-1080.webm",
                                        "resolution": 1080,
                                        "source": "BD"
                                    }
                                ]
                            }
                        ]
                    }
                ]
            }
        }"#;

        let actual =
            extract_animethemes_webm_from_api_json(json, "OP1").expect("api json should parse");
        assert_eq!(
            actual.as_deref(),
            Some("https://v.animethemes.moe/MeitanteiPrecure-OP1-1080.webm")
        );
    }

    #[test]
    fn matches_theme_using_type_and_sequence_when_slug_differs() {
        let json = r#"{
            "included": [
                {
                    "type": "animetheme",
                    "id": "14234",
                    "attributes": { "type": "OP", "sequence": 1 },
                    "relationships": {
                        "animethemeentries": {
                            "data": [{ "type": "animethemeentry", "id": "16647" }]
                        }
                    }
                },
                {
                    "type": "animethemeentry",
                    "id": "16647",
                    "relationships": {
                        "videos": { "data": [{ "type": "video", "id": "19396" }] }
                    }
                },
                {
                    "type": "video",
                    "id": "19396",
                    "attributes": {
                        "link": "https://v.animethemes.moe/MeitanteiPrecure-OP1.webm",
                        "resolution": 720,
                        "source": "WEB"
                    }
                }
            ]
        }"#;

        let actual =
            extract_animethemes_webm_from_api_json(json, "OP1v2").expect("api json should parse");
        assert_eq!(
            actual.as_deref(),
            Some("https://v.animethemes.moe/MeitanteiPrecure-OP1.webm")
        );
    }

    #[test]
    fn returns_none_when_target_theme_not_found() {
        let json = r#"{
            "anime": {
                "animethemes": [
                    {
                        "slug": "ED1",
                        "animethemeentries": [
                            {
                                "videos": [
                                    { "link": "https://v.animethemes.moe/MeitanteiPrecure-ED1.webm" }
                                ]
                            }
                        ]
                    }
                ]
            }
        }"#;

        let actual =
            extract_animethemes_webm_from_api_json(json, "OP1").expect("api json should parse");
        assert!(actual.is_none());
    }

    #[test]
    fn parses_total_size_from_content_range() {
        let headers = "HTTP/2 206\r\nContent-Range: bytes 0-0/48937934\r\nContent-Length: 1\r\n";
        assert_eq!(parse_content_range_total(headers), Some(48_937_934));
    }

    #[test]
    fn parses_content_length_normally() {
        let headers = "HTTP/2 200\r\nContent-Length: 75350559\r\n";
        assert_eq!(parse_content_length_from_headers(headers), Some(75_350_559));
    }
}
