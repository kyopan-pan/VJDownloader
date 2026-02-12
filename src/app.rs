use crate::bundled::ensure_bundled_tools;
use crate::download::{
    CANCELLED_ERROR, DownloadEvent, ProcessTracker, ProgressUpdate, ensure_deno, ensure_yt_dlp,
    read_clipboard_text, run_download,
};
use crate::fs_utils::{delete_download_file, is_executable, load_mp4_files};
use crate::log_ui;
use crate::mac_input_source::{InputMode, current_mode};
use crate::mac_menu;
use crate::paths::{search_index_db_path, yt_dlp_path};
use crate::search_index::{SearchEngine, SearchHit, SearchRequest, SearchSort};
use crate::settings::{SettingsData, load_cookie_args, save_settings};
use crate::settings_ui;
use crate::theme::apply_theme;
use crate::ui;
use crate::{app_logger::AppLogger, log_ui::LogUiState};
use drag::{DragItem, Image, Options};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

pub fn run() -> eframe::Result<()> {
    let settings = SettingsData::load();
    let window_width = settings.window_width.parse::<f32>().unwrap_or(860.0);
    let window_height = settings.window_height.parse::<f32>().unwrap_or(1000.0);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([window_width, window_height])
            .with_min_inner_size([640.0, 320.0])
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "YT Downloader",
        options,
        Box::new(|cc| Ok(Box::new(DownloaderApp::new(cc)))),
    )
}

#[derive(Clone)]
struct SearchJob {
    seq: u64,
    request: SearchRequest,
}

struct SearchJobResult {
    seq: u64,
    result: Result<Vec<SearchHit>, String>,
}

pub struct DownloaderApp {
    pub(crate) download_dir: PathBuf,
    pub(crate) downloaded_files: Vec<PathBuf>,
    pub(crate) download_in_progress: bool,
    pub(crate) progress_message: String,
    pub(crate) progress_value: f32,
    pub(crate) progress_visible: bool,
    pub(crate) download_active_flag: Arc<AtomicBool>,
    pub(crate) cancel_flag: Option<Arc<AtomicBool>>,
    pub(crate) process_tracker: Option<ProcessTracker>,
    pub(crate) rx: Option<mpsc::Receiver<DownloadEvent>>,
    pub(crate) last_scan: Instant,
    pub(crate) refresh_needed: bool,
    pub(crate) settings_ui: settings_ui::SettingsUiState,
    pub(crate) log_ui: LogUiState,
    pub(crate) status_logs: AppLogger,
    pub(crate) pending_window_resize: Option<egui::Vec2>,
    pub(crate) did_snap: bool,
    pub(crate) current_window_size: Option<egui::Vec2>,
    pub(crate) search_query: String,
    pub(crate) search_results: Vec<SearchHit>,
    pub(crate) search_error: Option<String>,
    pub(crate) search_engine: Option<SearchEngine>,
    pub(crate) search_roots_sync_error: Option<String>,
    search_job_tx: Option<mpsc::Sender<SearchJob>>,
    search_result_rx: Option<mpsc::Receiver<SearchJobResult>>,
    search_request_seq: u64,
    applied_search_seq: u64,
    search_dirty: bool,
    last_input_mode: Option<InputMode>,
}

impl DownloaderApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let settings = SettingsData::load();
        let download_dir = PathBuf::from(settings.download_dir.trim());
        let search_engine = SearchEngine::new(search_index_db_path()).ok();
        let mut search_roots_sync_error = None;

        if let Some(engine) = search_engine.as_ref() {
            let root_paths = settings
                .search_roots
                .iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            if let Err(err) = engine.sync_roots(&root_paths) {
                search_roots_sync_error = Some(err);
            }
            let _ = engine.reindex_all_async();
        }

        let (search_job_tx, search_result_rx) = if let Some(engine) = search_engine.clone() {
            let (job_tx, job_rx) = mpsc::channel::<SearchJob>();
            let (result_tx, result_rx) = mpsc::channel::<SearchJobResult>();
            thread::spawn(move || search_worker_loop(engine, job_rx, result_tx));
            (Some(job_tx), Some(result_rx))
        } else {
            (None, None)
        };

        let mut app = Self {
            download_dir,
            downloaded_files: Vec::new(),
            download_in_progress: false,
            progress_message: "待機中...".to_string(),
            progress_value: 0.0,
            progress_visible: false,
            download_active_flag: Arc::new(AtomicBool::new(false)),
            cancel_flag: None,
            process_tracker: None,
            rx: None,
            last_scan: Instant::now() - Duration::from_secs(5),
            refresh_needed: true,
            settings_ui: settings_ui::SettingsUiState::new(),
            log_ui: log_ui::LogUiState::new(),
            status_logs: AppLogger::new(),
            pending_window_resize: None,
            did_snap: false,
            current_window_size: None,
            search_query: String::new(),
            search_results: Vec::new(),
            search_error: None,
            search_engine,
            search_roots_sync_error,
            search_job_tx,
            search_result_rx,
            search_request_seq: 0,
            applied_search_seq: 0,
            search_dirty: true,
            last_input_mode: None,
        };

        mac_menu::install_settings_menu();

        if let Err(err) = ensure_bundled_tools() {
            app.push_status(format!("同梱ツールの配置に失敗しました: {err}"));
        }

        thread::spawn(|| {
            let _ = ensure_yt_dlp(None);
            let _ = ensure_deno(None);
        });

        if app.search_engine.is_none() {
            app.search_error = Some("検索エンジンの初期化に失敗しました。".to_string());
        }
        if let Some(err) = app.search_roots_sync_error.clone() {
            app.search_error = Some(format!("検索対象フォルダの同期に失敗しました: {err}"));
        }

        app
    }

    pub(crate) fn push_status(&mut self, message: impl Into<String>) {
        self.status_logs.push(message);
    }

    pub(crate) fn clear_logs(&mut self) {
        self.status_logs.clear();
    }

    pub(crate) fn build_recent_log_snapshot(&self, duration: Duration) -> String {
        self.status_logs.build_recent_snapshot(duration)
    }

    pub(crate) fn start_download_from_clipboard(&mut self) {
        let Some(url) = read_clipboard_text() else {
            return;
        };

        if !self.is_tools_ready() {
            self.push_status(
                "初回セットアップが必要です。設定から自動セットアップを行ってください。"
                    .to_string(),
            );
            self.settings_ui.open_initial_setup();
            return;
        }

        let output_dir = self.download_dir.clone();
        let cookie_args = load_cookie_args();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.download_in_progress = true;
        self.download_active_flag.store(true, Ordering::Relaxed);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let tracker = ProcessTracker::new();
        self.cancel_flag = Some(cancel_flag.clone());
        self.process_tracker = Some(tracker.clone());

        self.push_status(format!("Downloading to {}", output_dir.to_string_lossy()));

        let active_flag = self.download_active_flag.clone();
        thread::spawn(move || {
            run_download(
                url,
                output_dir,
                cookie_args,
                tx,
                active_flag,
                cancel_flag,
                tracker,
            )
        });
    }

    pub(crate) fn request_cancel_download(&mut self) {
        if let Some(flag) = self.cancel_flag.as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(tracker) = self.process_tracker.as_ref() {
            tracker.terminate_all();
        }
        self.progress_message = "キャンセル中...".to_string();
        self.progress_value = -1.0;
        self.progress_visible = true;
    }

    pub(crate) fn delete_download(&mut self, path: &Path) {
        match delete_download_file(path) {
            Ok(()) => {
                self.refresh_needed = true;
            }
            Err(err) => self.push_status(format!("削除に失敗しました: {err}")),
        }
    }

    pub(crate) fn start_native_drag(&mut self, frame: &eframe::Frame, path: &Path) {
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                self.push_status(format!("ドラッグ対象の取得に失敗しました: {err}"));
                return;
            }
        };

        let icon_path = match drag_preview_icon_path() {
            Some(path) => path,
            None => {
                self.push_status("ドラッグ用アイコンが見つかりません。".to_string());
                return;
            }
        };

        if let Err(err) = drag::start_drag(
            frame,
            DragItem::Files(vec![path]),
            Image::File(icon_path),
            |_result, _position| {},
            Options::default(),
        ) {
            self.push_status(format!("ドラッグ開始に失敗しました: {err}"));
        }
    }

    pub(crate) fn mark_search_dirty(&mut self) {
        self.search_dirty = true;
    }

    pub(crate) fn sync_search_roots(&mut self, roots: &[String]) -> Result<(), String> {
        let Some(engine) = self.search_engine.as_ref() else {
            return Err(
                "検索エンジンが初期化されていません。アプリを再起動してください。".to_string(),
            );
        };
        let paths = roots.iter().map(PathBuf::from).collect::<Vec<_>>();
        engine.sync_roots(&paths)?;
        self.search_roots_sync_error = None;
        self.search_dirty = true;
        Ok(())
    }

    pub(crate) fn request_reindex_all(&mut self) -> Result<(), String> {
        let Some(engine) = self.search_engine.as_ref() else {
            return Err("検索エンジンが初期化されていません。".to_string());
        };
        engine.reindex_all_async()?;
        self.search_dirty = true;
        Ok(())
    }

    fn poll_download_events(&mut self) {
        let mut events = Vec::new();
        if let Some(rx) = self.rx.as_ref() {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }

        let mut done = None;
        for event in events {
            match event {
                DownloadEvent::Log(line) => self.push_status(line),
                DownloadEvent::Progress(update) => self.handle_progress_update(update),
                DownloadEvent::Done(result) => done = Some(result),
            }
        }

        if let Some(result) = done {
            match result {
                Ok(()) => self.push_status("Download completed."),
                Err(err) if err == CANCELLED_ERROR => {
                    self.push_status("ダウンロードをキャンセルしました。".to_string())
                }
                Err(err) => self.push_status(format!("Download failed: {err}")),
            }
            self.download_in_progress = false;
            self.download_active_flag.store(false, Ordering::Relaxed);
            self.rx = None;
            self.cancel_flag = None;
            self.process_tracker = None;
            self.refresh_needed = true;
        }
    }

    fn refresh_downloads_if_needed(&mut self) {
        if self.refresh_needed || self.last_scan.elapsed() >= Duration::from_secs(2) {
            self.downloaded_files = load_mp4_files(&self.download_dir);
            self.last_scan = Instant::now();
            self.refresh_needed = false;
        }
    }

    fn handle_progress_update(&mut self, update: ProgressUpdate) {
        if update.visible {
            self.progress_message = update.message;
            self.progress_value = update.progress;
            self.progress_visible = true;
        } else {
            self.progress_message = "待機中...".to_string();
            self.progress_value = 0.0;
            self.progress_visible = false;
        }
    }

    fn is_yt_dlp_ready(&self) -> bool {
        let path = yt_dlp_path();
        path.exists() && is_executable(&path)
    }

    fn is_tools_ready(&self) -> bool {
        self.is_yt_dlp_ready()
    }

    fn poll_input_mode_change(&mut self) {
        let Some(mode) = current_mode() else {
            return;
        };

        if self.last_input_mode.is_none() {
            self.last_input_mode = Some(mode);
            return;
        }

        if self.last_input_mode.as_ref() == Some(&mode) {
            return;
        }

        self.last_input_mode = Some(mode.clone());
        match mode {
            InputMode::Japanese => self.push_status("日本語になりました".to_string()),
            InputMode::English => self.push_status("英字になりました".to_string()),
            InputMode::Other(name) => {
                self.push_status(format!("入力ソースが変更されました: {name}"))
            }
        }
    }

    fn submit_search_if_needed(&mut self) {
        if !self.search_dirty {
            return;
        }

        if self.search_query.trim().is_empty() {
            self.search_results.clear();
            let has_persistent_search_error =
                self.search_engine.is_none() || self.search_roots_sync_error.is_some();
            if !has_persistent_search_error {
                self.search_error = None;
            }
            self.search_dirty = false;
            return;
        }

        let Some(tx) = self.search_job_tx.as_ref() else {
            return;
        };

        self.search_request_seq = self.search_request_seq.saturating_add(1);
        let seq = self.search_request_seq;
        let sort = if self.search_query.trim().is_empty() {
            SearchSort::ModifiedDesc
        } else {
            SearchSort::NameAsc
        };
        let request = SearchRequest {
            query: self.search_query.clone(),
            limit: 200,
            sort,
            ..Default::default()
        };

        if tx.send(SearchJob { seq, request }).is_ok() {
            self.search_dirty = false;
        } else {
            self.search_error =
                Some("検索ワーカーにリクエストを送信できませんでした。".to_string());
        }
    }

    fn poll_search_results(&mut self) {
        let Some(rx) = self.search_result_rx.as_ref() else {
            return;
        };

        let mut latest_result = None;
        while let Ok(result) = rx.try_recv() {
            latest_result = Some(result);
        }

        let Some(result) = latest_result else {
            return;
        };
        if result.seq < self.applied_search_seq {
            return;
        }

        self.applied_search_seq = result.seq;
        match result.result {
            Ok(hits) => {
                self.search_results = hits;
                self.search_error = None;
            }
            Err(err) => {
                self.search_results.clear();
                self.search_error = Some(err);
            }
        }
    }
}

impl eframe::App for DownloaderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if mac_menu::take_open_settings_request() {
            self.settings_ui.open_settings();
        }
        if mac_menu::take_open_logs_request() {
            self.log_ui.open_logs();
        }
        self.current_window_size = ctx.input(|i| i.viewport().inner_rect.map(|rect| rect.size()));
        if let Some(size) = self.pending_window_resize.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
        if !self.did_snap {
            let (monitor_size, inner_rect) =
                ctx.input(|i| (i.viewport().monitor_size, i.viewport().inner_rect));
            if let (Some(monitor_size), Some(inner_rect)) = (monitor_size, inner_rect) {
                let margin = 12.0;
                let x = (monitor_size.x - inner_rect.width() - margin).max(0.0);
                let y = margin.max(0.0);
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
                self.did_snap = true;
            }
        }
        self.settings_ui.poll_tool_updates();
        self.settings_ui.auto_refresh_if_needed();
        self.poll_input_mode_change();
        self.poll_download_events();
        self.refresh_downloads_if_needed();
        self.poll_search_results();
        self.submit_search_if_needed();
        ui::render(self, ctx, _frame);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(size) = self.current_window_size {
            let mut data = SettingsData::load();
            data.window_width = format_dimension(size.x.max(640.0));
            data.window_height = format_dimension(size.y.max(320.0));
            let _ = save_settings(&data);
        }
    }
}

fn search_worker_loop(
    engine: SearchEngine,
    rx: mpsc::Receiver<SearchJob>,
    tx: mpsc::Sender<SearchJobResult>,
) {
    while let Ok(mut job) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            job = newer;
        }

        let result = engine.search(&job.request);
        if tx
            .send(SearchJobResult {
                seq: job.seq,
                result,
            })
            .is_err()
        {
            return;
        }
    }
}

fn format_dimension(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{:.0}", value)
    } else {
        format!("{value}")
    }
}

fn drag_preview_icon_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let path = PathBuf::from(
            "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericDocumentIcon.icns",
        );
        if path.exists() {
            return Some(path);
        }
    }
    None
}
