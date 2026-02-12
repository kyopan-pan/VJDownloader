use notify::event::ModifyKind;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use super::normalize::{epoch_millis, is_mp4_path, path_to_key};
use super::scanner::{
    build_record_from_path, find_root_id_for_path, trigger_reindex_all_from_db, upsert_directory,
};
use super::{
    DEBOUNCE_WINDOW, EngineResult, PendingChanges, WatchedRoot, WatcherMessage, WriteCommand,
};

// notify のイベントを受け取り、debounce 後に差分更新コマンドへ変換する。
pub(super) fn watcher_loop(
    rx: Receiver<WatcherMessage>,
    write_tx: Sender<WriteCommand>,
    db_path: PathBuf,
) {
    let (event_tx, event_rx) = mpsc::channel();
    let callback_tx = event_tx.clone();
    let mut watcher = match RecommendedWatcher::new(
        move |res| {
            let _ = callback_tx.send(res);
        },
        Config::default(),
    ) {
        Ok(watcher) => watcher,
        Err(err) => {
            eprintln!("[search-index] failed to create watcher: {err}");
            return;
        }
    };

    let mut watched_roots = Vec::<WatchedRoot>::new();
    let mut pending = PendingChanges::default();

    loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WatcherMessage::SetRoots(roots) => {
                    reset_watch_targets(&mut watcher, &mut watched_roots, roots);
                }
                WatcherMessage::Shutdown => return,
            }
        }

        match event_rx.recv_timeout(std::time::Duration::from_millis(150)) {
            Ok(Ok(event)) => {
                collect_pending_change(&mut pending, &event);
            }
            Ok(Err(err)) => {
                eprintln!("[search-index] watcher event error: {err}");
                trigger_reindex_all_from_db(&db_path, &write_tx);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        if should_flush_pending(&pending) {
            if let Err(err) = flush_pending_changes(&mut pending, &watched_roots, &write_tx) {
                eprintln!("[search-index] failed to flush watcher changes: {err}");
                trigger_reindex_all_from_db(&db_path, &write_tx);
            }
        }
    }
}

// 現在の watch 対象を一旦解除して、新しい root セットへ差し替える。
fn reset_watch_targets(
    watcher: &mut RecommendedWatcher,
    current: &mut Vec<WatchedRoot>,
    next: Vec<WatchedRoot>,
) {
    for root in current.iter() {
        if let Err(err) = watcher.unwatch(&root.root_path) {
            eprintln!(
                "[search-index] failed to unwatch {}: {}",
                root.root_path.to_string_lossy(),
                err
            );
        }
    }

    current.clear();
    for root in next {
        if !root.root_path.exists() {
            continue;
        }
        if let Err(err) = watcher.watch(&root.root_path, RecursiveMode::Recursive) {
            eprintln!(
                "[search-index] failed to watch {}: {}",
                root.root_path.to_string_lossy(),
                err
            );
            continue;
        }
        current.push(root);
    }
}

// rename と通常変更を切り分けて pending キューへ積む。
fn collect_pending_change(pending: &mut PendingChanges, event: &Event) {
    if matches!(event.kind, EventKind::Modify(ModifyKind::Name(_))) && event.paths.len() >= 2 {
        pending
            .moves
            .push((event.paths[0].clone(), event.paths[1].clone()));
        pending.last_change_at = Some(Instant::now());
        return;
    }

    for path in &event.paths {
        pending.path_changes.insert(path.clone());
    }
    pending.last_change_at = Some(Instant::now());
}

// 最終変更から debounce 窓を超えたら flush 対象とする。
fn should_flush_pending(pending: &PendingChanges) -> bool {
    if pending.path_changes.is_empty() && pending.moves.is_empty() {
        return false;
    }

    pending
        .last_change_at
        .map(|last| last.elapsed() >= DEBOUNCE_WINDOW)
        .unwrap_or(false)
}

// pending 変更を upsert/delete コマンドへまとめて変換する。
fn flush_pending_changes(
    pending: &mut PendingChanges,
    roots: &[WatchedRoot],
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    let mut delete_paths = HashSet::<String>::new();
    let mut delete_prefixes = HashSet::<String>::new();
    let mut upsert_paths = HashSet::<PathBuf>::new();

    for (old_path, new_path) in pending.moves.drain(..) {
        collect_delete_target(&old_path, &mut delete_paths, &mut delete_prefixes);
        upsert_paths.insert(new_path);
    }

    for path in pending.path_changes.drain() {
        upsert_paths.insert(path);
    }

    pending.last_change_at = None;

    for path in upsert_paths {
        if path.exists() {
            let metadata = match fs::metadata(&path) {
                Ok(meta) => meta,
                Err(_) => {
                    continue;
                }
            };

            if metadata.is_dir() {
                upsert_directory(&path, roots, write_tx)?;
                continue;
            }

            if !is_mp4_path(&path) {
                continue;
            }

            if let Some(root_id) = find_root_id_for_path(&path, roots) {
                if let Some(record) = build_record_from_path(root_id, &path, epoch_millis()) {
                    write_tx
                        .send(WriteCommand::UpsertFiles {
                            files: vec![record],
                        })
                        .map_err(|err| err.to_string())?;
                }
            }
        } else {
            collect_delete_target(&path, &mut delete_paths, &mut delete_prefixes);
        }
    }

    if !delete_paths.is_empty() {
        write_tx
            .send(WriteCommand::DeletePaths {
                paths: delete_paths.into_iter().collect(),
            })
            .map_err(|err| err.to_string())?;
    }

    if !delete_prefixes.is_empty() {
        write_tx
            .send(WriteCommand::DeleteByPrefixes {
                prefixes: delete_prefixes.into_iter().collect(),
            })
            .map_err(|err| err.to_string())?;
    }

    Ok(())
}

// 削除対象がファイルかディレクトリか不明な場合も含め、消し込みキーを収集する。
fn collect_delete_target(
    path: &Path,
    delete_paths: &mut HashSet<String>,
    delete_prefixes: &mut HashSet<String>,
) {
    let key = path_to_key(path);
    if path.exists() && path.is_dir() {
        delete_prefixes.insert(key);
        return;
    }

    if path.exists() {
        delete_paths.insert(key);
        return;
    }

    // 消失パスはファイル/ディレクトリ両方の可能性があるため、完全一致と配下一致を両方削除する。
    delete_paths.insert(key.clone());
    delete_prefixes.insert(key);
}

#[cfg(test)]
// テスト用: 削除変更を write コマンドへ直接変換する。
pub(super) fn apply_delete_change(
    old_path: &Path,
    _roots: &[WatchedRoot],
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    if old_path.is_dir() {
        write_tx
            .send(WriteCommand::DeleteByPrefixes {
                prefixes: vec![path_to_key(old_path)],
            })
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    write_tx
        .send(WriteCommand::DeletePaths {
            paths: vec![path_to_key(old_path)],
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

#[cfg(test)]
// テスト用: 追加/更新変更を write コマンドへ直接変換する。
pub(super) fn apply_upsert_change(
    new_path: &Path,
    roots: &[WatchedRoot],
    write_tx: &Sender<WriteCommand>,
) -> EngineResult<()> {
    if !new_path.exists() {
        return Ok(());
    }

    let metadata = fs::metadata(new_path).map_err(|err| err.to_string())?;
    if metadata.is_dir() {
        return upsert_directory(new_path, roots, write_tx);
    }

    if !is_mp4_path(new_path) {
        return Ok(());
    }

    let Some(root_id) = find_root_id_for_path(new_path, roots) else {
        return Ok(());
    };

    if let Some(record) = build_record_from_path(root_id, new_path, epoch_millis()) {
        write_tx
            .send(WriteCommand::UpsertFiles {
                files: vec![record],
            })
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}
