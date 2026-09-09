use std::collections::VecDeque;

use time::{Duration, OffsetDateTime};

use super::LogEntry;

/// ログ画面に保持する件数の上限。
const MAX_ENTRIES: usize = 1000;

/// ログ画面表示用のリングバッファ。ファイル出力とは独立しており、
/// ここから溢れた行もファイル側には残る。
pub struct AppLogger {
    entries: VecDeque<LogEntry>,
}

impl AppLogger {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(MAX_ENTRIES),
        }
    }

    pub fn push(&mut self, entry: LogEntry) {
        self.entries.push_back(entry);
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = &LogEntry> {
        self.entries.iter()
    }

    /// 直近 `duration` 分のログを1つの文字列へまとめる。単調増加の Instant ではなく実時刻で
    /// 判定するため、スリープを跨いでも画面の表示と範囲が一致する。
    pub fn build_recent_snapshot(&self, duration: Duration) -> String {
        if duration.is_zero() {
            return String::new();
        }

        let cutoff = OffsetDateTime::now_local()
            .unwrap_or_else(|_| OffsetDateTime::now_utc())
            .checked_sub(duration);
        let mut out = String::new();
        for entry in &self.entries {
            if let Some(cutoff) = cutoff
                && entry.at < cutoff
            {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&entry.display_line());
        }
        out
    }
}

impl Default for AppLogger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::{Level, Source};

    fn entry(at: OffsetDateTime, body: &str) -> LogEntry {
        LogEntry {
            at,
            level: Level::Info,
            source: Source::App,
            body: body.to_string(),
        }
    }

    #[test]
    fn keeps_only_latest_entries() {
        let mut logger = AppLogger::new();
        let now = OffsetDateTime::now_utc();
        for index in 0..(MAX_ENTRIES + 10) {
            logger.push(entry(now, &format!("行{index}")));
        }
        assert_eq!(logger.entries().count(), MAX_ENTRIES);
        let first = logger
            .entries()
            .next()
            .expect("上限に達していれば必ず先頭がある");
        assert_eq!(first.body, "行10", "古い行から捨てられていない");
    }

    #[test]
    fn snapshot_excludes_entries_older_than_duration() {
        let mut logger = AppLogger::new();
        let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        logger.push(entry(now - Duration::minutes(30), "古い行"));
        logger.push(entry(now - Duration::minutes(1), "新しい行"));

        let snapshot = logger.build_recent_snapshot(Duration::minutes(10));
        assert!(!snapshot.contains("古い行"), "範囲外の行が含まれている");
        assert!(snapshot.contains("新しい行"), "範囲内の行が欠けている");
    }
}
