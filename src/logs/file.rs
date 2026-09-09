use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

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

/// OpenTelemetry の Resource 属性 `os.type` に合わせた値。
#[cfg(target_os = "macos")]
const OS_TYPE: &str = "darwin";
#[cfg(target_os = "windows")]
const OS_TYPE: &str = "windows";

/// ファイル出力スレッドを起動し、送信口を返す。保存先を用意できない場合は None を返し、
/// ログ画面への出力だけで動作を続ける。
pub(super) fn spawn_writer() -> Option<mpsc::Sender<LogEntry>> {
    let dir = log_dir();
    if fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let (tx, rx) = mpsc::channel::<LogEntry>();
    // ディスク入出力を専用スレッドへ隔離し、UIスレッドが書き込み待ちで止まらないようにする。
    thread::spawn(move || writer_loop(dir, rx));
    Some(tx)
}

/// 現在書き込み中のログファイルのパス。ログ画面の案内表示に使う。
pub fn current_file_path() -> PathBuf {
    log_dir().join(file_name(today()))
}

fn writer_loop(dir: PathBuf, rx: mpsc::Receiver<LogEntry>) {
    let mut open: Option<(Date, File)> = None;
    while let Ok(entry) = rx.recv() {
        let date = entry.at.date();
        if open.as_ref().is_none_or(|(current, _)| *current != date) {
            // 起動直後と日付が変わったときだけ掃除すれば、常駐したままでも容量が増え続けない。
            purge_expired(&dir, date);
            open = open_file(&dir, date).map(|file| (date, file));
        }
        let Some((_, file)) = open.as_mut() else {
            continue;
        };
        let line = entry.file_line();
        // `cargo run` やターミナル起動で追えるように標準出力へも複製する。
        // 呼び出し元スレッドを止めないため、入出力はこのスレッドに集約している。
        println!("{line}");
        // クラッシュ時に直前の行を失わないよう、行ごとに書き切る。
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
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
}
