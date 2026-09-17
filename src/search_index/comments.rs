// フェーズ2: 動画コメントの後埋め（バックフィル）。
//
// フェーズ1（scanner.rs）はファイル名だけを即座にインデックスし、コメント列は NULL のまま残す。
// ここではその NULL 行を少しずつ拾い、ffprobe でコメントを読んで書き戻す。
// 10万件規模だと数時間かかる処理なので、次の性質を満たすように作ってある。
//
// - 検索・ダウンロードと並行して動く（専用スレッドと writer キュー経由の書き込み）
// - 途中で終了しても状態は DB 上（comment_norm IS NULL）にあるので次回続きから再開できる
// - ffprobe はプロセス起動とファイル読み取りの待ちが大半なので並列に走らせる
// - ffprobe を実行できなかった行は取得待ちのまま残し、後で取り直せるようにする

use rusqlite::Connection;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::paths::{ffprobe_path, ffprobe_ready};
use crate::platform::process::hidden_command;

use super::db::open_connection;
use super::normalize::normalize_for_search;
use super::{
    COMMENT_BATCH_SIZE, CommentRecord, EngineResult, FFPROBE_TIMEOUT, IndexProgress,
    MAX_COMMENT_WORKERS, WriteCommand, update_progress,
};

// 残件数が減らないまま許容する連続回数。
const STALLED_ROUND_LIMIT: u32 = 3;

#[derive(Debug)]
pub(super) enum BackfillMessage {
    // コメント取得待ちが増えたかもしれないので確認させる。
    Kick,
    Shutdown,
}

// コメント取得待ちの1件。ffprobe 実行中に実体が差し替わった行を上書きしないよう、
// 読み出した時点のサイズと更新日時を持ち回る。
#[derive(Clone, Debug)]
struct PendingFile {
    path: String,
    size_bytes: i64,
    modified_time: i64,
}

// コメント取得の失敗を、取り直す価値があるかどうかで分ける。
#[derive(Debug)]
enum ProbeError {
    // ffprobe 自体を動かせなかった（未導入・起動失敗・無応答・出力の読み取り失敗）。
    // 環境側の問題でファイルには何の問題もないため、取得待ちのまま残して後で取り直す。
    ToolUnavailable(String),
    // ffprobe は動いたが、そのファイルからは読めなかった。ファイル側の問題なので
    // 空コメントで確定させる。実体が変わればフェーズ1が取得待ちへ戻す。
    FileUnreadable(String),
}

impl ProbeError {
    fn message(&self) -> &str {
        match self {
            Self::ToolUnavailable(message) | Self::FileUnreadable(message) => message,
        }
    }
}

// バックフィル専用スレッドの本体。Kick を受けるたびに、取得待ちが尽きるまで処理する。
pub(super) fn comment_backfill_loop(
    rx: Receiver<BackfillMessage>,
    db_path: PathBuf,
    write_tx: Sender<WriteCommand>,
    progress: Arc<Mutex<IndexProgress>>,
    shutdown: Arc<AtomicBool>,
) {
    while let Ok(message) = rx.recv() {
        if matches!(message, BackfillMessage::Shutdown) || shutdown.load(Ordering::Relaxed) {
            return;
        }

        // 走査中に届いた Kick はここでまとめて捨てる。1回のパスで取得待ちを
        // 尽きるまで処理するため、同じ内容で走り直す必要はない。
        while let Ok(message) = rx.try_recv() {
            if matches!(message, BackfillMessage::Shutdown) {
                return;
            }
        }

        if let Err(err) = run_backfill_pass(&db_path, &write_tx, &progress, &shutdown) {
            crate::log_error!(Search, "動画コメントの取得に失敗しました: {err}");
        }

        update_progress(&progress, |progress| {
            progress.comment_running = false;
            progress.comment_done = 0;
            progress.comment_pending = 0;
        });
    }
}

// 取得待ちが尽きるまでバッチ処理を繰り返す。
fn run_backfill_pass(
    db_path: &Path,
    write_tx: &Sender<WriteCommand>,
    progress: &Mutex<IndexProgress>,
    shutdown: &AtomicBool,
) -> EngineResult<()> {
    let conn = open_connection(db_path)?;
    if count_pending(&conn)? == 0 {
        return Ok(());
    }

    // ffprobe 未導入のまま走らせると、全行を「読めなかった」として扱いかねない。
    // Windows は SearchEngine の起動後に ffprobe を取得するため、揃うまで何もしない。
    // 取得が終わった時点で app 側が改めて Kick する。
    if !ffprobe_ready() {
        crate::log_info!(
            Search,
            "ffprobeが見つからないため、動画コメントの取得を保留します。"
        );
        return Ok(());
    }

    let mut done: u64 = 0;
    let mut last_pending = u64::MAX;
    let mut stalled_rounds = 0;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        let pending_count = count_pending(&conn)?;
        if pending_count == 0 {
            return Ok(());
        }

        // 書き込み中のファイルなど、取得しても更新条件（サイズと更新日時の一致）を
        // 満たさない行は残り続ける。同じ行を延々と ffprobe にかけないよう、
        // 残件数が減らない状態が続いたらこのパスは打ち切る。次の Kick で再開する。
        if pending_count >= last_pending {
            stalled_rounds += 1;
            if stalled_rounds >= STALLED_ROUND_LIMIT {
                return Ok(());
            }
        } else {
            stalled_rounds = 0;
        }
        last_pending = pending_count;

        update_progress(progress, |progress| {
            progress.comment_running = true;
            progress.comment_done = done;
            progress.comment_pending = pending_count;
        });

        let batch = load_pending_batch(&conn, COMMENT_BATCH_SIZE)?;
        if batch.is_empty() {
            return Ok(());
        }

        let comments = probe_comments(batch, shutdown);
        if comments.is_empty() {
            // 中断、または ffprobe を動かせず全行を取得待ちへ残した場合。
            // ここで抜けないと同じ行を引き続ける。
            return Ok(());
        }
        // 取得待ちへ残した行はここに含まれないため、書き戻す件数だけを進捗へ反映する。
        let probed_len = comments.len() as u64;

        // 反映を待ってから次のバッチを引く。待たずに再検索すると、まだコミットされていない
        // 同じ行を引き当てて同じファイルを繰り返し ffprobe にかけ続けてしまう。
        let (resp_tx, resp_rx) = mpsc::channel();
        write_tx
            .send(WriteCommand::UpsertComments {
                comments,
                resp: resp_tx,
            })
            .map_err(|err| err.to_string())?;
        resp_rx.recv().map_err(|err| err.to_string())??;

        done = done.saturating_add(probed_len);
        update_progress(progress, |progress| {
            progress.comment_done = done;
            progress.comment_pending = progress.comment_pending.saturating_sub(probed_len);
        });
    }
}

fn count_pending(conn: &Connection) -> EngineResult<u64> {
    conn.query_row(
        "SELECT COUNT(*) FROM files WHERE comment_norm IS NULL",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count.max(0) as u64)
    .map_err(|err| err.to_string())
}

fn load_pending_batch(conn: &Connection, limit: usize) -> EngineResult<Vec<PendingFile>> {
    let mut stmt = conn
        .prepare(
            "SELECT path, size_bytes, modified_time
             FROM files
             WHERE comment_norm IS NULL
             LIMIT ?",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([limit as i64], |row| {
            Ok(PendingFile {
                path: row.get(0)?,
                size_bytes: row.get(1)?,
                modified_time: row.get(2)?,
            })
        })
        .map_err(|err| err.to_string())?;

    let mut files = Vec::new();
    for row in rows {
        files.push(row.map_err(|err| err.to_string())?);
    }
    Ok(files)
}

// バッチを複数スレッドへ分けて ffprobe にかける。
//
// 書き戻すレコードだけを返す。ffprobe を動かせなかった行は結果に含めず、
// 取得待ち（comment_norm IS NULL）のまま残して後の Kick で取り直す。
fn probe_comments(batch: Vec<PendingFile>, shutdown: &AtomicBool) -> Vec<CommentRecord> {
    let worker_count = comment_worker_count().min(batch.len().max(1));
    let cursor = AtomicUsize::new(0);
    let batch = &batch;

    thread::scope(|scope| {
        let handles = (0..worker_count)
            .map(|_| {
                scope.spawn(|| {
                    let mut results = Vec::new();
                    loop {
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        let index = cursor.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = batch.get(index) else {
                            break;
                        };

                        let probed = read_search_comment(Path::new(&file.path));
                        log_probe_failure(&file.path, &probed);
                        if let Some(record) = comment_record(file, probed) {
                            results.push(record);
                        }
                    }
                    results
                })
            })
            .collect::<Vec<_>>();

        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .flatten()
            .collect()
    })
}

// ffprobe の結果を書き戻すレコードへ変換する。取り直すべき失敗では None を返し、
// その行を取得待ちのまま残す。
fn comment_record(file: &PendingFile, probed: Result<String, ProbeError>) -> Option<CommentRecord> {
    let comment = match probed {
        Ok(comment) => comment,
        // ファイル側の問題は空コメントで確定させる。NULL のまま残すと次のバッチで
        // 同じ行を引き当て続けて先へ進めなくなる。
        Err(ProbeError::FileUnreadable(_)) => String::new(),
        Err(ProbeError::ToolUnavailable(_)) => return None,
    };

    Some(CommentRecord {
        path: file.path.clone(),
        size_bytes: file.size_bytes,
        modified_time: file.modified_time,
        comment_norm: normalize_for_search(&comment),
        comment,
    })
}

// 失敗の種類で扱いが変わるため、後で取り直すのかどうかもログへ残す。
fn log_probe_failure(path: &str, probed: &Result<String, ProbeError>) {
    match probed {
        Ok(_) => {}
        Err(err @ ProbeError::ToolUnavailable(_)) => crate::log_warn!(
            Search,
            "ffprobeを実行できなかったため、動画コメントは後で取り直します: {path} ({})",
            err.message()
        ),
        Err(err @ ProbeError::FileUnreadable(_)) => crate::log_warn!(
            Search,
            "動画コメントの読み取りに失敗しました: {path} ({})",
            err.message()
        ),
    }
}

fn comment_worker_count() -> usize {
    thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .clamp(1, MAX_COMMENT_WORKERS)
}

// ffprobeからコメント系タグを取得する。タグ名の大文字・小文字は区別しない。
fn read_search_comment(path: &Path) -> Result<String, ProbeError> {
    let mut child = hidden_command(ffprobe_path())
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format_tags")
        .arg("-of")
        .arg("json")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ProbeError::ToolUnavailable(err.to_string()))?;

    let Some((stdout, stderr)) = read_child_output(&mut child, FFPROBE_TIMEOUT) else {
        // 応答しない ffprobe を放置するとバックフィルがそのファイルで止まったままになる。
        let _ = child.kill();
        let _ = child.wait();
        return Err(ProbeError::ToolUnavailable(format!(
            "ffprobeが{}秒以内に応答しませんでした",
            FFPROBE_TIMEOUT.as_secs()
        )));
    };

    let status = child
        .wait()
        .map_err(|err| ProbeError::ToolUnavailable(err.to_string()))?;
    if !status.success() {
        // ffprobe は動いているので、読めないのはファイル側の問題として扱う。
        return Err(ProbeError::FileUnreadable(
            String::from_utf8_lossy(&stderr).trim().to_string(),
        ));
    }

    let value: Value = serde_json::from_slice(&stdout)
        .map_err(|err| ProbeError::FileUnreadable(err.to_string()))?;
    Ok(parse_search_comment(&value))
}

// 子プロセスの stdout / stderr を読み切って返す。制限時間内に閉じなければ None。
//
// パイプだけを別スレッドへ移し、Child は呼び出し側に残す。こうすると待ち受け側は
// タイムアウト時にそのまま kill でき、stdout と stderr を並行して読むので
// どちらかのパイプバッファが埋まってデッドロックすることもない。
fn read_child_output(child: &mut Child, timeout: Duration) -> Option<(Vec<u8>, Vec<u8>)> {
    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    }

    let stdout_rx = drain(child.stdout.take());
    let stderr_rx = drain(child.stderr.take());

    let stdout = stdout_rx.recv_timeout(timeout).ok()?;
    let stderr = stderr_rx.recv_timeout(timeout).ok()?;
    Some((stdout, stderr))
}

fn parse_search_comment(value: &Value) -> String {
    let Some(tags) = value
        .get("format")
        .and_then(|format| format.get("tags"))
        .and_then(Value::as_object)
    else {
        return String::new();
    };

    let mut values = Vec::<String>::new();
    for (key, value) in tags {
        if !key.eq_ignore_ascii_case("comment") && !key.eq_ignore_ascii_case("description") {
            continue;
        }
        let Some(text) = value
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        if !values.iter().any(|existing| existing == text) {
            values.push(text.to_string());
        }
    }
    values.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        MAX_COMMENT_WORKERS, PendingFile, ProbeError, comment_record, comment_worker_count,
        parse_search_comment,
    };

    fn pending_file() -> PendingFile {
        PendingFile {
            path: "/videos/a.mp4".to_string(),
            size_bytes: 100,
            modified_time: 10,
        }
    }

    #[test]
    fn parses_comment_and_description_tags_case_insensitively() {
        let value = json!({
            "format": {
                "tags": {
                    "COMMENT": "YouTubeの概要欄",
                    "description": "補足説明",
                    "title": "検索対象外"
                }
            }
        });

        assert_eq!(parse_search_comment(&value), "YouTubeの概要欄\n補足説明");
    }

    #[test]
    fn stores_normalized_comment_when_probe_succeeds() {
        let record = comment_record(&pending_file(), Ok("ライブ映像".to_string()))
            .expect("取得できた行は書き戻す");
        assert_eq!(record.comment, "ライブ映像");
        assert_eq!(record.comment_norm, "らいぶ映像");
    }

    #[test]
    fn stores_empty_comment_when_file_is_unreadable() {
        // ffprobe は動いたが読めなかったファイル。取得待ちに残すと同じ行を引き続ける。
        let record = comment_record(
            &pending_file(),
            Err(ProbeError::FileUnreadable(
                "moov atom not found".to_string(),
            )),
        )
        .expect("読めなかった行は空コメントで確定させる");
        assert_eq!(record.comment, "");
        assert_eq!(record.comment_norm, "");
    }

    #[test]
    fn keeps_row_pending_when_ffprobe_cannot_run() {
        // ffprobe 未導入やタイムアウトで空コメントを確定させると、
        // その行のコメント検索が恒久的に欠落する。
        assert!(
            comment_record(
                &pending_file(),
                Err(ProbeError::ToolUnavailable(
                    "No such file or directory".to_string()
                )),
            )
            .is_none()
        );
    }

    #[test]
    fn keeps_worker_count_within_bounds() {
        let count = comment_worker_count();
        assert!((1..=MAX_COMMENT_WORKERS).contains(&count));
    }
}
