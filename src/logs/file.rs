use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration as StdDuration;

use time::macros::format_description;
use time::{Date, Duration, OffsetDateTime};

use super::{LogEntry, format_timestamp};
use crate::paths::log_dir;

/// ログファイルの保持日数。これより古い日付のファイルは起動時と日付切り替え時に削除する。
const RETENTION_DAYS: i64 = 40;
const FILE_PREFIX: &str = "app-";
const FILE_SUFFIX: &str = ".log";
/// ファイル名の日付スタンプ（`YYYY-MM-DD`）の桁数。
const STAMP_LEN: usize = 10;
/// パニック処理を長時間止めず、それでも通常のディスク書き込みを待てる上限。
const PANIC_FLUSH_TIMEOUT: StdDuration = StdDuration::from_secs(1);

/// OpenTelemetry の Resource 属性 `os.type` に合わせた値。
#[cfg(target_os = "macos")]
const OS_TYPE: &str = "darwin";
#[cfg(target_os = "windows")]
const OS_TYPE: &str = "windows";

enum WriterMessage {
    Entry(LogEntry),
    WriteAndFlush(LogEntry, mpsc::Sender<bool>),
    Shutdown(mpsc::Sender<()>),
}

/// 非同期ログの送信口と終了待ち用のスレッドハンドル。
pub(super) struct FileWriter {
    tx: mpsc::Sender<WriterMessage>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl FileWriter {
    pub fn write(&self, entry: LogEntry) {
        if let Err(err) = self.tx.send(WriterMessage::Entry(entry)) {
            write_emergency(&err.0.into_entry());
        }
    }

    /// パニックの記録だけは書き込み完了を待つ。writer 自身が停止している場合は同期追記へ退避する。
    pub fn write_panic(&self, entry: LogEntry) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self
            .tx
            .send(WriterMessage::WriteAndFlush(entry.clone(), ack_tx))
            .is_err()
            || !ack_rx.recv_timeout(PANIC_FLUSH_TIMEOUT).unwrap_or(false)
        {
            write_emergency(&entry);
        }
    }

    /// キュー済みのログをすべて書き切ってから writer を終了する。
    pub fn shutdown(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(WriterMessage::Shutdown(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(PANIC_FLUSH_TIMEOUT);
        }
        if let Ok(mut handle) = self.handle.lock()
            && let Some(handle) = handle.take()
        {
            let _ = handle.join();
        }
    }
}

impl WriterMessage {
    fn into_entry(self) -> LogEntry {
        match self {
            Self::Entry(entry) | Self::WriteAndFlush(entry, _) => entry,
            Self::Shutdown(_) => unreachable!("終了通知にログ本文は含まれない"),
        }
    }
}

/// ファイル出力スレッドを起動する。保存先を用意できない場合は None を返し、
/// ログ画面と標準エラーへの出力だけで動作を続ける。
pub(super) fn spawn_writer() -> Option<FileWriter> {
    let dir = log_dir();
    if fs::create_dir_all(&dir).is_err() {
        return None;
    }
    Some(spawn_writer_in(dir))
}

fn spawn_writer_in(dir: PathBuf) -> FileWriter {
    // ディスク入出力を専用スレッドへ隔離し、UIスレッドが書き込み待ちで止まらないようにする。
    let (tx, rx) = mpsc::channel::<WriterMessage>();
    let handle = thread::spawn(move || writer_loop(dir, rx));
    FileWriter {
        tx,
        handle: Mutex::new(Some(handle)),
    }
}

/// 現在書き込み中のログファイルのパス。ログ画面の案内表示に使う。
pub fn current_file_path() -> PathBuf {
    log_dir().join(file_name(today()))
}

fn writer_loop(dir: PathBuf, rx: mpsc::Receiver<WriterMessage>) {
    let mut open: Option<(Date, File)> = None;
    while let Ok(message) = rx.recv() {
        match message {
            WriterMessage::Entry(entry) => {
                write_entry(&dir, &mut open, &entry);
            }
            WriterMessage::WriteAndFlush(entry, ack) => {
                let written = write_entry(&dir, &mut open, &entry);
                let _ = ack.send(written);
            }
            WriterMessage::Shutdown(ack) => {
                if let Some((_, file)) = open.as_mut() {
                    let _ = file.flush();
                }
                let _ = ack.send(());
                break;
            }
        }
    }
}

fn write_entry(dir: &Path, open: &mut Option<(Date, File)>, entry: &LogEntry) -> bool {
    let date = entry.at.date();
    if open.as_ref().is_none_or(|(current, _)| *current != date) {
        // 起動直後と日付が変わったときだけ掃除すれば、常駐したままでも容量が増え続けない。
        purge_expired(dir, date);
        *open = open_file(dir, date).map(|file| (date, file));
    }
    let line = entry.file_line();
    let Some((_, file)) = open.as_mut() else {
        eprintln!("ログファイルを開けないため標準エラーへ退避します: {line}");
        return false;
    };
    // `cargo run` やターミナル起動で追えるように標準出力へも複製する。
    println!("{line}");
    // クラッシュ時に直前の行を失わないよう、行ごとに書き切る。
    if writeln!(file, "{line}").and_then(|_| file.flush()).is_err() {
        eprintln!("ログファイルへ書き込めないため標準エラーへ退避します: {line}");
        // 一時的なファイル障害なら次のレコードで開き直せるよう、壊れたハンドルを捨てる。
        *open = None;
        return false;
    }
    true
}

/// 非同期 writer が利用できない場合の最終退避。パニック中でも新たな panic を起こさない。
pub(super) fn write_emergency(entry: &LogEntry) {
    let dir = log_dir();
    let line = entry.file_line();
    if fs::create_dir_all(&dir).is_ok()
        && let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(file_name(entry.at.date())))
    {
        let needs_header = file.metadata().is_ok_and(|metadata| metadata.len() == 0);
        let header_result = if needs_header {
            writeln!(file, "{}", session_header())
        } else {
            Ok(())
        };
        if header_result
            .and_then(|_| writeln!(file, "{line}"))
            .and_then(|_| file.flush())
            .is_ok()
        {
            return;
        }
    }
    eprintln!("緊急ログをファイルへ書き込めませんでした: {line}");
}

fn open_file(dir: &Path, date: Date) -> Option<File> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(file_name(date)))
        .ok()?;
    // OpenTelemetry の Resource 相当。ファイルを開くたびに書くことで、
    // 同じ日に再起動した場合でもセッションの境界が分かる。
    let _ = writeln!(file, "{}", session_header());
    Some(file)
}

fn session_header() -> String {
    format!(
        "# service.name=VJDownloader service.version={} os.type={} session.start={}",
        env!("CARGO_PKG_VERSION"),
        OS_TYPE,
        format_timestamp(now())
    )
}

/// 保持期間を過ぎたログファイルを削除する。
fn purge_expired(dir: &Path, today: Date) {
    let Some(cutoff) = today.checked_sub(Duration::days(RETENTION_DAYS)) else {
        return;
    };
    let Ok(cutoff) = cutoff.format(&stamp_format()) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stamp) = name.to_str().and_then(date_stamp) else {
            continue;
        };
        // `YYYY-MM-DD` は固定桁なので、文字列の大小比較がそのまま日付の前後比較になる。
        // time クレートの parsing フィーチャーを増やさずに済ませるためこの形にしている。
        if stamp < cutoff.as_str() {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// ログファイル名から日付スタンプを取り出す。無関係なファイルを削除しないよう書式も検査する。
fn date_stamp(name: &str) -> Option<&str> {
    let stamp = name.strip_prefix(FILE_PREFIX)?.strip_suffix(FILE_SUFFIX)?;
    if stamp.len() != STAMP_LEN {
        return None;
    }
    let shaped = stamp
        .as_bytes()
        .iter()
        .enumerate()
        .all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    shaped.then_some(stamp)
}

fn file_name(date: Date) -> String {
    let stamp = date
        .format(&stamp_format())
        .unwrap_or_else(|_| "unknown".to_string());
    format!("{FILE_PREFIX}{stamp}{FILE_SUFFIX}")
}

fn stamp_format() -> time::format_description::StaticFormatDescription {
    format_description!("[year]-[month]-[day]")
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc())
}

fn today() -> Date {
    now().date()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(body: &str) -> LogEntry {
        LogEntry {
            at: OffsetDateTime::now_utc(),
            level: crate::logs::Level::Error,
            source: crate::logs::Source::App,
            body: body.to_string(),
        }
    }

    #[test]
    fn date_stamp_accepts_only_log_file_names() {
        assert_eq!(date_stamp("app-2026-09-09.log"), Some("2026-09-09"));
        assert_eq!(date_stamp("app-2026-09-9.log"), None, "桁数が違う");
        assert_eq!(date_stamp("app-20260909.log"), None, "区切りが無い");
        assert_eq!(date_stamp("settings.properties"), None, "無関係なファイル");
        assert_eq!(date_stamp("app-unknown.log"), None, "日付が不明なファイル");
    }

    #[test]
    fn purge_removes_only_files_older_than_retention() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作成できる");
        let today = Date::from_calendar_date(2026, time::Month::September, 9)
            .expect("2026-09-09は有効な日付");
        let expired = today
            .checked_sub(Duration::days(RETENTION_DAYS + 1))
            .expect("保持期間を超える日付を作れる");
        let kept = today
            .checked_sub(Duration::days(RETENTION_DAYS - 1))
            .expect("保持期間内の日付を作れる");

        for date in [today, kept, expired] {
            fs::write(dir.path().join(file_name(date)), b"x").expect("ログファイルを作成できる");
        }
        // 日付形式でないファイルは保持期間の判定対象外であることも確認する。
        fs::write(dir.path().join("readme.txt"), b"x").expect("無関係なファイルを作成できる");

        purge_expired(dir.path(), today);

        assert!(dir.path().join(file_name(today)).exists(), "当日分が消えた");
        assert!(
            dir.path().join(file_name(kept)).exists(),
            "保持期間内のファイルが消えた"
        );
        assert!(
            !dir.path().join(file_name(expired)).exists(),
            "保持期間を過ぎたファイルが残っている"
        );
        assert!(
            dir.path().join("readme.txt").exists(),
            "無関係なファイルを削除した"
        );
    }

    #[test]
    fn panic_write_is_visible_before_returning() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作成できる");
        let writer = spawn_writer_in(dir.path().to_path_buf());
        let entry = test_entry("panicの記録");
        let path = dir.path().join(file_name(entry.at.date()));

        writer.write_panic(entry);

        let contents = fs::read_to_string(path).expect("完了通知時点でログを読み取れる");
        assert!(contents.contains("panicの記録"));
        writer.shutdown();
    }

    #[test]
    fn shutdown_drains_queued_entries() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作成できる");
        let writer = spawn_writer_in(dir.path().to_path_buf());
        let entry = test_entry("終了直前の記録");
        let path = dir.path().join(file_name(entry.at.date()));

        writer.write(entry);
        writer.shutdown();

        let contents = fs::read_to_string(path).expect("終了後にログを読み取れる");
        assert!(contents.contains("終了直前の記録"));
    }

    #[test]
    fn write_failure_is_reported_to_caller() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作成できる");
        let not_a_directory = dir.path().join("file");
        fs::write(&not_a_directory, b"x").expect("通常ファイルを作成できる");
        let mut open = None;

        assert!(!write_entry(
            &not_a_directory,
            &mut open,
            &test_entry("退避対象")
        ));
    }
}
