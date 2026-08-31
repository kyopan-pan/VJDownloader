use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::DownloadEvent;

// 検出したBot対策の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardKind {
    // 403/429/Bot判定など、リクエスト自体が拒否された状態。
    Blocked,
    // スリープやリトライ待機など、処理は続いているが遅延している状態。
    Throttled,
}

// yt-dlp/ffmpeg のログ行から検出したBot対策の通知。
#[derive(Clone, Debug)]
pub struct GuardNotice {
    pub kind: GuardKind,
    pub message: String,
    // 待機秒数を読み取れた場合のみ設定する。
    pub wait_seconds: Option<f32>,
}

// 警告表示を維持する最小時間。短い待機でも視認できるようにする。
const MIN_NOTICE_HOLD: Duration = Duration::from_secs(12);
// Bot対策由来の失敗表示（赤ボタン）を維持する時間。
const RESTRICTED_HOLD: Duration = Duration::from_secs(600);

// ログ 1 行を判定し、Bot対策に該当する場合のみ通知イベントを送る。
pub(super) fn notify(line: &str, tx: &mpsc::Sender<DownloadEvent>) {
    if let Some(notice) = detect(line) {
        let _ = tx.send(DownloadEvent::Guard(notice));
    }
}

// ログ 1 行からBot対策の兆候を判定する。拒否判定を遅延判定より優先する。
pub(super) fn detect(line: &str) -> Option<GuardNotice> {
    let lower = line.to_lowercase();

    if let Some(message) = detect_blocked(&lower) {
        return Some(GuardNotice {
            kind: GuardKind::Blocked,
            message,
            wait_seconds: None,
        });
    }

    detect_throttled(&lower).map(|(message, wait_seconds)| GuardNotice {
        kind: GuardKind::Throttled,
        message,
        wait_seconds,
    })
}

// リクエストが拒否された（＝ダウンロードが進まない）行を判定する。
// 動画タイトルやファイル名に同じ語が含まれても誤検出しないよう、エラー文脈の行だけを対象にする。
fn detect_blocked(lower: &str) -> Option<String> {
    if !has_error_context(lower) {
        return None;
    }
    if lower.contains("not a bot") || lower.contains("sign in to confirm") {
        return Some("Bot判定されました（サインイン確認が要求されています）".to_string());
    }
    if lower.contains("captcha") {
        return Some("CAPTCHAが要求されました".to_string());
    }
    if has_status_code(lower, "403") {
        return Some("アクセスを拒否されました（HTTP 403）".to_string());
    }
    if has_status_code(lower, "429") || lower.contains("too many requests") {
        return Some("リクエスト過多で拒否されました（HTTP 429）".to_string());
    }
    if lower.contains("this content isn't available")
        || lower.contains("this content isn’t available")
    {
        return Some("一時的にコンテンツを取得できませんでした".to_string());
    }
    None
}

// 処理は継続しているが待たされている行を判定する。
fn detect_throttled(lower: &str) -> Option<(String, Option<f32>)> {
    if lower.contains("sleeping") {
        let wait = extract_wait_seconds(lower);
        let message = match wait {
            Some(seconds) => format!("Bot制限のため{seconds:.0}秒待機中・・・"),
            None => "Bot制限のため待機中・・・".to_string(),
        };
        return Some((message, wait));
    }
    if lower.contains("retrying") || lower.contains("retry ") {
        let wait = extract_wait_seconds(lower);
        return Some(("接続を再試行しています".to_string(), wait));
    }
    if lower.contains("throttl") {
        return Some(("転送速度が制限されています".to_string(), None));
    }
    if lower.contains("rate limit") {
        return Some(("レート制限を検出しました".to_string(), None));
    }
    None
}

// yt-dlp/curl が失敗を報告している行かどうかを判定する。
fn has_error_context(lower: &str) -> bool {
    lower.contains("error")
        || lower.contains("warning")
        || lower.contains("unable")
        || lower.contains("failed")
        || lower.contains("giving up")
}

// 数字だけの誤検出を避けるため、HTTPステータスとして現れる形だけを拾う。
fn has_status_code(lower: &str, code: &str) -> bool {
    [
        format!("http error {code}"),
        format!("http error: {code}"),
        format!("http {code}"),
        format!("status code {code}"),
        format!("status code: {code}"),
        format!("status: {code}"),
        format!("returned {code}"),
        format!("error {code}:"),
        format!("error: {code}"),
        format!("{code} forbidden"),
        format!("{code} too many requests"),
    ]
    .iter()
    .any(|pattern| lower.contains(pattern.as_str()))
}

// "sleeping 5.00 seconds" のような表記から待機秒数を取り出す。
fn extract_wait_seconds(lower: &str) -> Option<f32> {
    let idx = lower.find("second")?;
    let token = lower[..idx].split_whitespace().last()?;
    let cleaned = token.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
    let value = cleaned.parse::<f32>().ok()?;
    if value.is_finite() && value >= 0.0 {
        Some(value)
    } else {
        None
    }
}

// Bot対策の検出結果をUI表示用に保持する状態。
#[derive(Default)]
pub struct BotGuardState {
    // 実行中のダウンロードで拒否を検出したか。
    detected_in_run: bool,
    // 実行中のダウンロードがYouTube向けか。表示文言の出し分けに使う。
    youtube: bool,
    // 進行度バーに出す警告。
    notice: Option<ActiveNotice>,
    // 失敗が確定し、ボタンを赤くしている状態。
    restriction: Option<Restriction>,
}

struct ActiveNotice {
    message: String,
    expires_at: Instant,
}

// Bot対策が原因でダウンロードが失敗したことを表す状態。
pub struct Restriction {
    pub message: String,
    // YouTube由来かどうか。ボタン文言の出し分けに使う。
    pub youtube: bool,
    pub at: Instant,
}

impl Restriction {
    pub fn label(&self) -> &'static str {
        if self.youtube {
            "YouTube制限中"
        } else {
            "サイト制限中"
        }
    }
}

impl BotGuardState {
    pub fn new() -> Self {
        Self::default()
    }

    // 新しいダウンロード開始時に前回の検出結果を破棄する。
    pub fn begin_run(&mut self, youtube: bool) {
        self.detected_in_run = false;
        self.youtube = youtube;
        self.notice = None;
        self.restriction = None;
    }

    // 検出通知を受け取り、警告表示の保持期限を更新する。
    pub fn observe(&mut self, notice: &GuardNotice) {
        if notice.kind == GuardKind::Blocked {
            self.detected_in_run = true;
        }
        // 待機秒数が分かる場合は、その待機が明けるまで警告を残す。
        let hold = notice
            .wait_seconds
            .map(|seconds| Duration::from_secs_f32(seconds + 3.0))
            .filter(|duration| *duration > MIN_NOTICE_HOLD)
            .unwrap_or(MIN_NOTICE_HOLD);
        self.notice = Some(ActiveNotice {
            message: notice.message.clone(),
            expires_at: Instant::now() + hold,
        });
    }

    // ダウンロード終了時に、Bot対策由来の失敗かどうかを確定させる。
    pub fn finish_run(&mut self, failed: bool) {
        let last_message = self.notice.take().map(|notice| notice.message);
        if failed && self.detected_in_run {
            self.restriction = Some(Restriction {
                message: last_message
                    .unwrap_or_else(|| "Bot対策によりアクセスを拒否されました".to_string()),
                youtube: self.youtube,
                at: Instant::now(),
            });
        }
        self.detected_in_run = false;
    }

    // 進行度バーに表示する警告文。期限切れなら None。
    // YouTubeの場合のみ`YTの`を前置し、どのサイトの制限かを明示する。
    pub fn warning(&self) -> Option<String> {
        let notice = self.notice.as_ref()?;
        if Instant::now() >= notice.expires_at {
            return None;
        }
        Some(if self.youtube {
            format!("YTの{}", notice.message)
        } else {
            notice.message.clone()
        })
    }

    // ボタンを赤くする制限状態。期限切れなら None。
    pub fn restriction(&self) -> Option<&Restriction> {
        let restriction = self.restriction.as_ref()?;
        (restriction.at.elapsed() < RESTRICTED_HOLD).then_some(restriction)
    }
}

// URLがYouTube（YouTube Music/短縮URLを含む）かどうかを判定する。
pub fn is_youtube_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("youtube.com") || lower.contains("youtu.be") || lower.contains("ytimg.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    // 通常のダウンロードログを警告として拾わないこと（タイトル由来の誤検出を含む）。
    #[test]
    fn ignores_ordinary_download_lines() {
        let lines = [
            "[youtube] Extracting URL: https://www.youtube.com/watch?v=abc123",
            "[youtube] abc123: Downloading webpage",
            "[info] abc123: Downloading 1 format(s): 137+140",
            "[download] Destination: /Users/x/Movies/Sample 403 Forbidden City.f137.mp4",
            "[download] Destination: /Users/x/Movies/Too Many Requests.f137.mp4",
            "[download]   0.0% of ~  12.34MiB at  Unknown B/s ETA Unknown",
            "[download]  52.9% of ~  12.34MiB at   3.21MiB/s ETA 00:02",
            "[download] 100% of   12.34MiB in 00:00:04 at 2.89MiB/s",
            "[Merger] Merging formats into \"/Users/x/Movies/Sample.mp4\"",
            "Deleting original file /Users/x/Movies/Sample.f137.mp4 (pass -k to keep)",
        ];
        for line in lines {
            assert!(detect(line).is_none(), "false positive: {line}");
        }
    }

    // Bot対策による拒否として扱う行。
    #[test]
    fn detects_blocked_lines() {
        let lines = [
            "ERROR: [youtube] abc123: Sign in to confirm you\u{2019}re not a bot. Use --cookies-from-browser",
            "ERROR: [youtube] abc123: Sign in to confirm you're not a bot",
            "WARNING: unable to download video data: HTTP Error 403: Forbidden",
            "ERROR: unable to download webpage: HTTP Error 429: Too Many Requests",
            "ERROR: [youtube] abc123: This content isn't available, try again later.",
            "ERROR: fragment 3 not found, unable to continue: HTTP error 403",
            "curl: (22) The requested URL returned error: 403",
        ];
        for line in lines {
            let notice = detect(line).unwrap_or_else(|| panic!("missed block: {line}"));
            assert_eq!(notice.kind, GuardKind::Blocked, "wrong kind: {line}");
        }
    }

    // 遅延（待機・再試行）として扱う行。
    #[test]
    fn detects_throttled_lines() {
        let lines = [
            "[download] Sleeping 3.00 seconds as required by the site...",
            "[download] Sleeping 1.25 seconds ...",
            "[download] Got error: HTTP Error 500: Internal Server Error. Retrying (attempt 1 of 10)...",
            "WARNING: [youtube] YouTube said: The download is throttled",
        ];
        for line in lines {
            let notice = detect(line).unwrap_or_else(|| panic!("missed throttle: {line}"));
            assert_eq!(notice.kind, GuardKind::Throttled, "wrong kind: {line}");
        }
    }

    // 待機秒数を読み取れる場合は警告文と保持時間に反映する。
    #[test]
    fn extracts_wait_seconds_from_sleep_line() {
        let notice = detect("[download] Sleeping 5.00 seconds as required by the site...")
            .expect("sleep should be detected");
        assert_eq!(notice.wait_seconds, Some(5.0));
        assert_eq!(notice.message, "Bot制限のため5秒待機中・・・");
    }

    // YouTubeの待機ログは`YTのBot制限のためn秒待機中・・・`として表示する。
    #[test]
    fn youtube_sleep_warning_uses_yt_wording() {
        let notice = detect("[download] Sleeping 6.00 seconds as required by the site...")
            .expect("sleep should be detected");

        let mut state = BotGuardState::new();
        state.begin_run(true);
        state.observe(&notice);
        assert_eq!(
            state.warning().as_deref(),
            Some("YTのBot制限のため6秒待機中・・・")
        );

        // YouTube以外では`YTの`を付けない。
        let mut state = BotGuardState::new();
        state.begin_run(false);
        state.observe(&notice);
        assert_eq!(
            state.warning().as_deref(),
            Some("Bot制限のため6秒待機中・・・")
        );
    }

    // 403を含む行でも、エラー文脈がなければ拒否として扱わない。
    #[test]
    fn ignores_bare_number_403() {
        assert!(detect("[download] Destination: 403 days later.mp4").is_none());
    }

    #[test]
    fn recognizes_youtube_urls() {
        assert!(is_youtube_url("https://www.youtube.com/watch?v=abc123"));
        assert!(is_youtube_url("https://youtu.be/abc123"));
        assert!(is_youtube_url("https://music.youtube.com/watch?v=abc123"));
        assert!(!is_youtube_url("https://animethemes.moe/anime/xxx"));
    }

    // 拒否を検出したまま失敗した場合のみボタン用の制限状態を持つ。
    #[test]
    fn restriction_is_set_only_for_failed_run_with_block() {
        let blocked = GuardNotice {
            kind: GuardKind::Blocked,
            message: "アクセスを拒否されました（HTTP 403）".to_string(),
            wait_seconds: None,
        };

        let mut state = BotGuardState::new();
        state.begin_run(true);
        state.observe(&blocked);
        state.finish_run(true);
        let restriction = state.restriction().expect("restriction should be set");
        assert_eq!(restriction.label(), "YouTube制限中");
        assert!(state.warning().is_none());

        // 成功した実行では制限状態を残さない。
        state.begin_run(true);
        state.observe(&blocked);
        state.finish_run(false);
        assert!(state.restriction().is_none());

        // 遅延だけの失敗は制限状態にしない。
        state.begin_run(true);
        state.observe(&GuardNotice {
            kind: GuardKind::Throttled,
            message: "Bot制限のため3秒待機中・・・".to_string(),
            wait_seconds: Some(3.0),
        });
        assert_eq!(
            state.warning().as_deref(),
            Some("YTのBot制限のため3秒待機中・・・")
        );
        state.finish_run(true);
        assert!(state.restriction().is_none());
    }

    // YouTube以外は別文言にする。
    #[test]
    fn restriction_label_differs_for_non_youtube() {
        let mut state = BotGuardState::new();
        state.begin_run(false);
        state.observe(&GuardNotice {
            kind: GuardKind::Blocked,
            message: "アクセスを拒否されました（HTTP 403）".to_string(),
            wait_seconds: None,
        });
        state.finish_run(true);
        assert_eq!(
            state.restriction().expect("restriction").label(),
            "サイト制限中"
        );
    }
}
