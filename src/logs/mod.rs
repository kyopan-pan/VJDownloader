pub mod file;
pub mod ring;
pub mod ui;

use std::sync::{Arc, Mutex, OnceLock, mpsc};

use time::OffsetDateTime;
use time::macros::format_description;

pub use ring::AppLogger;

/// ログの重大度。OpenTelemetry Logs Data Model の SeverityNumber / SeverityText に対応する。
/// デスクトップアプリでは TRACE と FATAL を出す機会がないため、4段だけを持つ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    /// OpenTelemetry の SeverityNumber。各レンジの下端（DEBUG=5 / INFO=9 / WARN=13 / ERROR=17）を使う。
    pub fn severity_number(self) -> u8 {
        match self {
            Level::Debug => 5,
            Level::Info => 9,
            Level::Warn => 13,
            Level::Error => 17,
        }
    }

    /// OpenTelemetry の SeverityText。
    pub fn severity_text(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// ログの発生源。OpenTelemetry の InstrumentationScope 名に相当する。
/// 文字列ではなく列挙型にして、発生源の表記揺れとタイプミスを防ぐ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    App,
    Setup,
    Download,
    Convert,
    Search,
}

impl Source {
    pub fn scope_name(self) -> &'static str {
        match self {
            Source::App => "app",
            Source::Setup => "setup",
            Source::Download => "download",
            Source::Convert => "convert",
            Source::Search => "search",
        }
    }
}

/// ログ1件。OpenTelemetry Logs Data Model のうち、単一プロセスのGUIアプリで意味を持つ
/// Timestamp / SeverityNumber / SeverityText / InstrumentationScope / Body だけを持つ。
/// TraceId・SpanId は分散トレースが無いため持たず、Resource はファイル先頭の1行で表す。
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub at: OffsetDateTime,
    pub level: Level,
    pub source: Source,
    pub body: String,
}

impl LogEntry {
    /// ファイル出力用の1行。
    pub fn file_line(&self) -> String {
        format!(
            "{}  {:<9}  {:<8}  {}",
            format_timestamp(self.at),
            format!(
                "{}({})",
                self.level.severity_text(),
                self.level.severity_number()
            ),
            self.source.scope_name(),
            self.body
        )
    }

    /// ログ画面用の1行。日付はウィンドウの幅を食うため省き、等幅フォントで桁が揃う幅に整える。
    pub fn display_line(&self) -> String {
        format!(
            "{}  {:<5}  {:<8}  {}",
            format_time_of_day(self.at),
            self.level.severity_text(),
            self.source.scope_name(),
            self.body
        )
    }
}

/// RFC 3339（ミリ秒・ローカルオフセット付き）。日付を持つため日跨ぎでも一意に読める。
pub fn format_timestamp(at: OffsetDateTime) -> String {
    at.format(&format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3][offset_hour sign:mandatory]:[offset_minute]"
    ))
    .unwrap_or_default()
}

fn format_time_of_day(at: OffsetDateTime) -> String {
    at.format(&format_description!("[hour]:[minute]:[second]"))
        .unwrap_or_else(|_| "00:00:00".to_string())
}

/// ローカル時刻。タイムゾーンを取得できない環境ではUTCへ倒す。
fn now_local() -> OffsetDateTime {
    OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc())
}

/// 外部ツール（yt-dlp / ffmpeg）の出力行からレベルを推定する。
/// 両ツールとも診断行に `ERROR:` / `WARNING:` を付けるため、その慣習に乗る。
pub fn classify_tool_line(line: &str) -> Level {
    let head = line.trim_start();
    if head.starts_with("ERROR") {
        Level::Error
    } else if head.starts_with("WARNING") || head.starts_with("WARN") {
        Level::Warn
    } else {
        Level::Info
    }
}

/// ログの集約点。出力先（ログ画面のリングとファイル）をここだけが知る。
struct LogHub {
    ring: Arc<Mutex<AppLogger>>,
    file_tx: Option<mpsc::Sender<LogEntry>>,
}

static HUB: OnceLock<LogHub> = OnceLock::new();

/// 起動時に一度だけ呼び、ログ画面へ渡すリングを受け取る。二度目以降は同じリングを返す。
pub fn init() -> Arc<Mutex<AppLogger>> {
    let hub = HUB.get_or_init(|| LogHub {
        ring: Arc::new(Mutex::new(AppLogger::new())),
        file_tx: file::spawn_writer(),
    });
    Arc::clone(&hub.ring)
}

/// ログを1件記録する。呼び出し側はロガーの参照を持たず、マクロ経由でここへ集約する。
pub fn emit(level: Level, source: Source, body: impl Into<String>) {
    let body = body.into();
    if body.is_empty() {
        return;
    }
    let entry = LogEntry {
        at: now_local(),
        level,
        source,
        body,
    };

    let Some(hub) = HUB.get() else {
        // init 前の呼び出しでも落とさない。開発時に気付けるよう標準エラーへは残す。
        eprintln!("{}", entry.file_line());
        return;
    };

    // 標準出力への複製は書き込みスレッド側で行う。ここで入出力を行うと、
    // 呼び出し元（多くはUIスレッド）が書き込み待ちで止まりうる。
    if let Some(tx) = hub.file_tx.as_ref() {
        let _ = tx.send(entry.clone());
    }
    if let Ok(mut ring) = hub.ring.lock() {
        ring.push(entry);
    }
}

/// パニック専用の記録口。リングのロックを取らず、ファイルへだけ書く。
/// ログ画面の描画中（リングのロック保持中）にパニックしても自己デッドロックしないようにする。
pub fn emit_panic(body: impl Into<String>) {
    let entry = LogEntry {
        at: now_local(),
        level: Level::Error,
        source: Source::App,
        body: body.into(),
    };
    if let Some(tx) = HUB.get().and_then(|hub| hub.file_tx.as_ref()) {
        let _ = tx.send(entry);
    }
}

#[macro_export]
macro_rules! log_error {
    ($source:ident, $($arg:tt)*) => {
        $crate::logs::emit(
            $crate::logs::Level::Error,
            $crate::logs::Source::$source,
            ::std::format!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! log_warn {
    ($source:ident, $($arg:tt)*) => {
        $crate::logs::emit(
            $crate::logs::Level::Warn,
            $crate::logs::Source::$source,
            ::std::format!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! log_info {
    ($source:ident, $($arg:tt)*) => {
        $crate::logs::emit(
            $crate::logs::Level::Info,
            $crate::logs::Source::$source,
            ::std::format!($($arg)*),
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_follows_opentelemetry_ranges() {
        // OpenTelemetry の SeverityNumber レンジ（DEBUG 5-8 / INFO 9-12 / WARN 13-16 / ERROR 17-20）。
        assert_eq!(Level::Debug.severity_number(), 5);
        assert_eq!(Level::Info.severity_number(), 9);
        assert_eq!(Level::Warn.severity_number(), 13);
        assert_eq!(Level::Error.severity_number(), 17);
    }

    #[test]
    fn file_line_starts_with_rfc3339_timestamp() {
        let entry = LogEntry {
            at: OffsetDateTime::from_unix_timestamp(0)
                .expect("エポックは常に有効")
                .to_offset(time::UtcOffset::from_hms(9, 0, 0).expect("+09:00は有効")),
            level: Level::Error,
            source: Source::Download,
            body: "失敗しました".to_string(),
        };
        let line = entry.file_line();
        assert!(
            line.starts_with("1970-01-01T09:00:00.000+09:00"),
            "RFC 3339形式になっていない: {line}"
        );
        assert!(line.contains("ERROR(17)"), "重大度が欠けている: {line}");
        assert!(line.contains("download"), "発生源が欠けている: {line}");
    }

    #[test]
    fn tool_lines_are_classified_by_prefix() {
        assert_eq!(
            classify_tool_line("ERROR: unable to download"),
            Level::Error
        );
        assert_eq!(classify_tool_line("WARNING: falling back"), Level::Warn);
        assert_eq!(classify_tool_line("[download] 50.0%"), Level::Info);
    }
}
