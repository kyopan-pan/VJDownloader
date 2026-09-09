#[cfg(not(target_os = "windows"))]
use crate::bundled::ensure_bundled_tools;
use crate::converter::{ConversionEvent, ConverterUiHandle, render_converter_viewport};
use crate::download::{
    BotGuardState, CANCELLED_ERROR, DownloadEvent, DownloadMode, ProcessTracker, ProgressUpdate,
    ensure_deno, ensure_yt_dlp, is_youtube_url, read_clipboard_text, run_download,
};
use crate::fs_utils::{delete_download_file, is_executable, load_mp4_files};
use crate::logs::ui::LogUiState;
use crate::logs::{self, AppLogger};
use crate::paths::{search_index_db_path, yt_dlp_path};
use crate::platform::input_source::{InputMode, current_mode};
use crate::platform::menu as mac_menu;
use crate::platform::window as mac_window;
use crate::search_index::{
    IndexEvent, IndexEventTarget, SearchEngine, SearchHit, SearchRequest, SearchSort,
};
use crate::settings::ui as settings_ui;
use crate::settings::{SettingsData, cookie_args_from_settings, save_settings};
use crate::speed_test as speed_test_ui;
use crate::speed_test::SpeedTestUiState;
use crate::stream::ui as stream_ui;
use crate::stream::ui::StreamUiState;
use crate::theme::apply_theme;
use crate::ui;
use crate::{log_error, log_info, log_warn};
use drag::{DragItem, Image, Options};
use eframe::egui;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

pub fn run() -> eframe::Result<()> {
    // ログの集約点はウィンドウを開く前に用意する。起動途中の失敗も記録できるようにする。
    logs::init();
    install_panic_hook();
    // 何も起きない起動ではログファイルが1件も作られないため、起動自体を1行残す。
    // セッションの開始位置とバージョンの手掛かりにもなる。
    log_info!(
        App,
        "VJDownloader {} を起動しました",
        env!("CARGO_PKG_VERSION")
    );
    let settings = SettingsData::load();
    let window_width = settings.window_width.parse::<f32>().unwrap_or(860.0);
    let window_height = settings.window_height.parse::<f32>().unwrap_or(1000.0);
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([window_width, window_height])
        .with_min_inner_size([320.0, 320.0])
        .with_always_on_top();
    #[cfg(target_os = "macos")]
    let viewport = viewport.with_icon(egui::IconData::default());
    // Windows はアプリバンドルが無く、指定しないと eframe 既定の egui ロゴが
    // タスクバーとタイトルバーに出るため、実行時にも自前のアイコンを渡す。
    #[cfg(target_os = "windows")]
    let viewport = viewport.with_icon(window_icon());
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let result = eframe::run_native(
        "VJDownloader",
        options,
        Box::new(|cc| Ok(Box::new(DownloaderApp::new(cc)))),
    );
    // 静的な送信口はプロセス終了まで残るため、明示的にキューを排出して writer を待つ。
    logs::shutdown();
    result
}

/// ワーカースレッドの panic は既定では標準エラーへ出るだけで、コンソールを持たない
/// リリースビルドでは失われる。ログファイルへ残し、後から原因を追えるようにする。
fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let thread = thread::current();
        let name = thread.name().unwrap_or("(名前なし)").to_string();
        // ログ画面のリングは触らない。描画中（ロック保持中）の panic で自己デッドロックしないため。
        logs::emit_panic(format!("panicが発生しました [thread: {name}] {info}"));
        previous(info);
    }));
}

/// タスクバー・タイトルバー用のアイコン。
/// eframe が必要なサイズへ縮小するため、素材は最大解像度の256pxを渡す。
#[cfg(target_os = "windows")]
fn window_icon() -> egui::IconData {
    const ICON_PNG: &[u8] = include_bytes!("../assets/icon/App.iconset/icon_256x256.png");
    // 埋め込み画像なので実行時に壊れることはなく、失敗時は既定アイコンで起動を続ける。
    eframe::icon_data::from_png_bytes(ICON_PNG).unwrap_or_default()
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
    pub(crate) bot_guard: BotGuardState,
    pub(crate) last_scan: Instant,
    pub(crate) refresh_needed: bool,
    pub(crate) settings_ui: settings_ui::SettingsUiHandle,
    pub(crate) cookie_args: Vec<String>,
    pub(crate) download_mode: DownloadMode,
    pub(crate) log_ui: Arc<Mutex<LogUiState>>,
    pub(crate) speed_test_ui: Arc<Mutex<SpeedTestUiState>>,
    pub(crate) stream_ui: Arc<Mutex<StreamUiState>>,
    pub(crate) converter_ui: ConverterUiHandle,
    pub(crate) status_logs: Arc<Mutex<AppLogger>>,
    pub(crate) pending_window_resize: Option<egui::Vec2>,
    pub(crate) did_snap: bool,
    pub(crate) current_window_size: Option<egui::Vec2>,
    pub(crate) download_panel_width: f32,
    pub(crate) search_panel_width: f32,
    pub(crate) search_query: String,
    pub(crate) search_results: Vec<SearchHit>,
    pub(crate) search_error: Option<String>,
    pub(crate) search_engine: Option<SearchEngine>,
    pub(crate) search_roots_sync_error: Option<String>,
    pub(crate) index_update_state: IndexUpdateState,
    index_event_rx: Option<mpsc::Receiver<IndexEvent>>,
    active_index_updates: usize,
    index_update_error: Option<String>,
    search_job_tx: Option<mpsc::Sender<SearchJob>>,
    search_result_rx: Option<mpsc::Receiver<SearchJobResult>>,
    search_request_seq: u64,
    applied_search_seq: u64,
    search_dirty: bool,
    last_input_mode: Option<InputMode>,
    last_focus_state: Option<bool>,
}

pub(crate) enum IndexUpdateState {
    Idle,
    Updating,
    Succeeded(Instant),
    Failed { at: Instant, message: String },
}

impl DownloaderApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let settings = SettingsData::load();
        let window_width = settings.window_width.parse::<f32>().unwrap_or(860.0);
        let download_dir = PathBuf::from(settings.download_dir.trim());
        let download_panel_width = settings
            .download_panel_width
            .parse::<f32>()
            .unwrap_or(window_width * 0.5);
        let search_panel_width = settings
            .search_panel_width
            .parse::<f32>()
            .unwrap_or(window_width * 0.5);
        // Windowsのffmpeg/ffprobeは起動時のバックグラウンド取得に任せるため、ここでは確認しない。
        #[cfg(not(target_os = "windows"))]
        let bundled_tools_error = ensure_bundled_tools().err();
        #[cfg(target_os = "windows")]
        let bundled_tools_error: Option<String> = None;
        let search_engine = SearchEngine::new(search_index_db_path()).ok();
        let index_event_rx = search_engine
            .as_ref()
            .map(SearchEngine::subscribe_index_events);
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
            if let Err(err) = engine.reindex_all_async() {
                log_error!(
                    Search,
                    "検索インデックスの初期更新を開始できませんでした: {err}"
                );
            }
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
            converter_ui: ConverterUiHandle::new(download_dir.clone()),
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
            bot_guard: BotGuardState::new(),
            last_scan: Instant::now() - Duration::from_secs(5),
            refresh_needed: true,
            settings_ui: settings_ui::SettingsUiHandle::new(),
            cookie_args: cookie_args_from_settings(&settings),
            download_mode: settings.download_mode,
            log_ui: Arc::new(Mutex::new(LogUiState::new())),
            speed_test_ui: Arc::new(Mutex::new(SpeedTestUiState::new())),
            stream_ui: Arc::new(Mutex::new(StreamUiState::new())),
            status_logs: logs::init(),
            pending_window_resize: None,
            did_snap: false,
            current_window_size: None,
            download_panel_width,
            search_panel_width,
            search_query: String::new(),
            search_results: Vec::new(),
            search_error: None,
            search_engine,
            search_roots_sync_error,
            index_update_state: IndexUpdateState::Idle,
            index_event_rx,
            active_index_updates: 0,
            index_update_error: None,
            search_job_tx,
            search_result_rx,
            search_request_seq: 0,
            applied_search_seq: 0,
            search_dirty: true,
            last_input_mode: None,
            last_focus_state: None,
        };

        mac_menu::install_settings_menu();
        mac_window::apply_app_icon_from_icns();

        if let Some(err) = bundled_tools_error {
            log_error!(Setup, "同梱ツールの配置に失敗しました: {err}");
        }

        // ロガーはグローバルな集約点を参照するため、スレッドへ参照を渡す必要がない。
        thread::spawn(move || {
            if let Err(err) = ensure_yt_dlp() {
                log_error!(Setup, "yt-dlpのセットアップに失敗しました: {err}");
            }
            if let Err(err) = ensure_deno() {
                log_error!(Setup, "Denoのセットアップに失敗しました: {err}");
            }
            // Windowsではffmpeg/ffprobeも取得対象になる。
            #[cfg(target_os = "windows")]
            if let Err(err) = crate::download::ensure_ffmpeg_tools() {
                log_error!(Setup, "ffmpeg/ffprobeのセットアップに失敗しました: {err}");
            }
        });

        if app.search_engine.is_none() {
            app.search_error = Some("検索エンジンの初期化に失敗しました。".to_string());
        }
        if let Some(err) = app.search_roots_sync_error.clone() {
            app.search_error = Some(format!("検索対象フォルダの同期に失敗しました: {err}"));
        }

        app
    }

    pub(crate) fn start_download_from_clipboard(&mut self) {
        let Some(url) = read_clipboard_text() else {
            return;
        };

        if !self.is_tools_ready() {
            log_warn!(
                Setup,
                "初回セットアップが必要です。設定から自動セットアップを行ってください。"
            );
            self.settings_ui.open_initial_setup();
            return;
        }

        // 前回のBot対策検出（赤ボタン・警告）は再試行の開始時点で解除する。
        self.bot_guard.begin_run(is_youtube_url(&url));

        let output_dir = self.download_dir.clone();
        let cookie_args = self.cookie_args.clone();
        let download_mode = self.download_mode;
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.download_in_progress = true;
        self.download_active_flag.store(true, Ordering::Relaxed);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let tracker = ProcessTracker::new();
        self.cancel_flag = Some(cancel_flag.clone());
        self.process_tracker = Some(tracker.clone());

        log_info!(Download, "Downloading to {}", output_dir.to_string_lossy());

        let active_flag = self.download_active_flag.clone();
        thread::spawn(move || {
            run_download(
                url,
                output_dir,
                cookie_args,
                download_mode,
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
            Err(err) => log_error!(App, "削除に失敗しました: {err}"),
        }
    }

    pub(crate) fn start_native_drag(&mut self, frame: &eframe::Frame, path: &Path) {
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                log_error!(App, "ドラッグ対象の取得に失敗しました: {err}");
                return;
            }
        };

        if let Err(err) = drag::start_drag(
            frame,
            DragItem::Files(vec![path]),
            drag_preview_image(),
            |_result, _position| {},
            Options::default(),
        ) {
            log_error!(App, "ドラッグ開始に失敗しました: {err}");
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
        engine.sync_roots_from_settings(&paths)?;
        self.search_roots_sync_error = None;
        self.search_dirty = true;
        Ok(())
    }

    pub(crate) fn request_reindex_all(&mut self) -> Result<usize, String> {
        let Some(engine) = self.search_engine.as_ref() else {
            return Err("検索エンジンが初期化されていません。".to_string());
        };
        let root_count = engine.reindex_all_from_settings_async()?;
        self.search_dirty = true;
        Ok(root_count)
    }

    fn poll_download_events(&mut self) {
        let mut events = Vec::new();
        if let Some(rx) = self.rx.as_ref() {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }

        // キャンセルで強制終了したプロセスが出すBot対策相当の通知は無視する。
        // 外部ツールの出力ログ自体は送出側（ProgressContext::is_cancelling）で抑制している。
        let cancelling = self
            .cancel_flag
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed));

        let mut done = None;
        for event in events {
            match event {
                DownloadEvent::Progress(update) => self.handle_progress_update(update),
                DownloadEvent::Guard(notice) => {
                    if !cancelling {
                        self.bot_guard.observe(&notice);
                    }
                }
                DownloadEvent::Done(result, elapsed) => done = Some((result, elapsed)),
            }
        }

        if let Some((result, elapsed)) = done {
            // キャンセルはBot対策由来の失敗として扱わない。
            let failed = matches!(&result, Err(err) if err != CANCELLED_ERROR);
            self.bot_guard.finish_run(failed);
            match result {
                Ok(()) => log_info!(Download, "Download completed. Total time: {elapsed}"),
                Err(err) if err == CANCELLED_ERROR => {
                    log_info!(Download, "ダウンロードをキャンセルしました。")
                }
                Err(err) => log_error!(Download, "Download failed: {err}"),
            }
            let restriction_status = self.bot_guard.restriction().map(|restriction| {
                format!(
                    "{}: {}。数分待ってから再試行してください。",
                    restriction.label(),
                    restriction.message
                )
            });
            if let Some(message) = restriction_status {
                log_warn!(Download, "{message}");
            }
            self.download_in_progress = false;
            self.download_active_flag.store(false, Ordering::Relaxed);
            self.rx = None;
            self.cancel_flag = None;
            self.process_tracker = None;
            self.refresh_needed = true;
        }
    }

    fn poll_conversion_events(&mut self) {
        while let Ok(event) = self.converter_ui.try_recv_event() {
            match event {
                ConversionEvent::Completed(_path) => {
                    log_info!(
                        Convert,
                        "MP4変換が完了しました: {}",
                        _path.to_string_lossy()
                    );
                    self.refresh_needed = true;
                }
                ConversionEvent::Failed(error) => {
                    log_error!(Convert, "MP4変換に失敗しました: {error}");
                }
            }
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
            InputMode::Japanese => log_info!(App, "日本語になりました"),
            InputMode::English => log_info!(App, "英字になりました"),
            InputMode::Other(name) => {
                log_info!(App, "入力ソースが変更されました: {name}")
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

    fn poll_index_events(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        if let Some(rx) = self.index_event_rx.as_ref() {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }

        for event in events {
            match event {
                IndexEvent::UpdateStarted {
                    target: IndexEventTarget::Main,
                } => {
                    self.active_index_updates = self.active_index_updates.saturating_add(1);
                    self.index_update_error = None;
                    self.index_update_state = IndexUpdateState::Updating;
                }
                IndexEvent::UpdateFinished {
                    target: IndexEventTarget::Main,
                    result,
                } => {
                    self.active_index_updates = self.active_index_updates.saturating_sub(1);
                    if let Err(error) = result {
                        self.index_update_error = Some(error);
                    }
                    if self.active_index_updates == 0 {
                        self.search_dirty = true;
                        self.index_update_state = match self.index_update_error.take() {
                            Some(message) => IndexUpdateState::Failed {
                                at: Instant::now(),
                                message,
                            },
                            None => IndexUpdateState::Succeeded(Instant::now()),
                        };
                    }
                }
                IndexEvent::UpdateStarted {
                    target: IndexEventTarget::Settings,
                } => self.settings_ui.send_index_started(),
                IndexEvent::UpdateFinished {
                    target: IndexEventTarget::Settings,
                    result,
                } => {
                    self.search_dirty = true;
                    self.settings_ui.send_index_finished(result);
                }
            }
        }

        if !matches!(self.index_update_state, IndexUpdateState::Idle) {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn maintain_cursor_tracking(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        let focus_changed = self.last_focus_state != Some(focused);
        self.last_focus_state = Some(focused);

        // Cocoa normally keeps this flag once it has been enabled. Re-enumerating every
        // window at 60 Hz after each focus change made secondary viewports stutter.
        if focus_changed {
            mac_window::enable_mouse_move_events_for_all_windows(true);
        }
    }
}

impl eframe::App for DownloaderApp {
    fn ui(&mut self, root_ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = root_ui.ctx().clone();
        self.maintain_cursor_tracking(&ctx);
        #[cfg(target_os = "windows")]
        mac_menu::render_context_menu(root_ui);
        if mac_menu::take_open_settings_request() {
            self.settings_ui.open_settings();
        }
        if mac_menu::take_open_logs_request()
            && let Ok(mut state) = self.log_ui.lock()
        {
            state.open_logs();
        }
        if mac_menu::take_open_speed_test_request()
            && let Ok(mut state) = self.speed_test_ui.lock()
        {
            state.open_speed_test();
        }
        if mac_menu::take_open_stream_request()
            && let Ok(mut state) = self.stream_ui.lock()
        {
            state.open_stream();
        }
        if mac_menu::take_open_converter_request() {
            self.converter_ui.open();
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
        if let Ok(mut state) = self.speed_test_ui.lock() {
            state.poll_updates();
        }
        if let Ok(mut state) = self.stream_ui.lock() {
            state.poll_updates();
        }
        settings_ui::process_requests(self, &ctx);
        self.poll_input_mode_change();
        self.poll_download_events();
        self.poll_conversion_events();
        self.refresh_downloads_if_needed();
        self.poll_search_results();
        self.poll_index_events(&ctx);
        self.submit_search_if_needed();
        ui::render(self, root_ui, frame);
        speed_test_ui::render_speed_test_viewport(&self.speed_test_ui, &ctx);
        stream_ui::render_stream_viewport(&self.stream_ui, self.cookie_args.clone(), &ctx);
        render_converter_viewport(&self.converter_ui, &ctx);
    }

    fn on_exit(&mut self) {
        if let Ok(mut stream) = self.stream_ui.lock() {
            stream.stop_all();
        }
        let mut data = SettingsData::load();
        if let Some(size) = self.current_window_size {
            data.window_width = format_dimension(size.x.max(320.0));
            data.window_height = format_dimension(size.y.max(320.0));
        }
        data.download_panel_width = format_dimension(self.download_panel_width.max(1.0));
        data.search_panel_width = format_dimension(self.search_panel_width.max(1.0));
        if let Err(err) = save_settings(&data) {
            log_error!(App, "終了時の設定保存に失敗しました: {err}");
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

fn drag_preview_image() -> Image {
    #[cfg(target_os = "macos")]
    {
        let path = PathBuf::from(
            "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericDocumentIcon.icns",
        );
        if path.exists() {
            return Image::File(path);
        }
    }

    // Windowsでは実行時の作業ディレクトリに依存しないよう、PNGを実行ファイルへ埋め込む。
    // macOSでもシステムの書類アイコンが見つからない場合は同じ画像を使用する。
    // App.iconset ではなく専用の画像を使う。アプリアイコンはDockでの見た目を
    // 他アプリと揃えるため周囲に余白を持つが、ドラッグ画像に余白は不要なため。
    Image::Raw(include_bytes!("../assets/icon/drag_32x32.png").to_vec())
}
