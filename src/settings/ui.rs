use eframe::egui;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::app::DownloaderApp;
use crate::cursor::pointing;
use crate::download::{DownloadMode, ensure_deno, ensure_yt_dlp, update_deno, update_yt_dlp};
use crate::fs_utils::is_executable;
use crate::paths::{default_download_dir, deno_path, make_absolute_path, yt_dlp_path};
use crate::platform::file_dialog as mac_file_dialog;
use crate::settings::{
    ChromeProfile, SettingsData, cookie_args_from_settings, load_chrome_profiles, save_settings,
};
use crate::theme::paint_viewport_background;

#[derive(Clone, Copy, Debug)]
enum ToolKind {
    YtDlp,
    Deno,
}

#[derive(Clone, Debug)]
struct ToolState {
    version: String,
    status: String,
    busy: bool,
    available: bool,
}

#[derive(Clone, Debug)]
struct ToolUpdate {
    kind: ToolKind,
    state: ToolState,
}

#[derive(Clone, Debug)]
struct SettingsForm {
    data: SettingsData,
    last_saved_data: SettingsData,
    dirty: bool,
    error: Option<String>,
}

pub enum SettingsAction {
    Save(SettingsData),
    Reindex,
}

enum SettingsResult {
    Saved(SettingsData),
    Error(String),
    Reindexed(Result<usize, String>),
    IndexStarted,
    IndexFinished(Result<(), String>),
}

pub struct SettingsUiHandle {
    state: Arc<Mutex<SettingsUiState>>,
    settings_visible: Arc<AtomicBool>,
    initial_setup_visible: Arc<AtomicBool>,
    action_rx: mpsc::Receiver<SettingsAction>,
    result_tx: mpsc::Sender<SettingsResult>,
}

impl SettingsForm {
    fn load() -> Self {
        let data = SettingsData::load();
        Self {
            data: data.clone(),
            last_saved_data: data,
            dirty: false,
            error: None,
        }
    }

    fn mark_changed(&mut self) {
        self.dirty = true;
    }
}

pub struct SettingsUiState {
    form: SettingsForm,
    yt_dlp: ToolState,
    deno: ToolState,
    tool_tx: mpsc::Sender<ToolUpdate>,
    tool_rx: mpsc::Receiver<ToolUpdate>,
    chrome_profiles: Vec<ChromeProfile>,
    action_tx: mpsc::Sender<SettingsAction>,
    result_rx: mpsc::Receiver<SettingsResult>,
    index_update_state: SettingsIndexUpdateState,
    active_index_updates: usize,
    index_update_error: Option<String>,
}

enum SettingsIndexUpdateState {
    Idle,
    Updating,
    Succeeded(Instant),
    Failed { at: Instant, message: String },
}

impl SettingsUiHandle {
    pub fn new() -> Self {
        let (tool_tx, tool_rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let yt_dlp = ToolState::from_disk(ToolKind::YtDlp);
        let needs_initial_setup = !yt_dlp.available;
        let deno = ToolState::from_disk(ToolKind::Deno);
        let handle = Self {
            state: Arc::new(Mutex::new(SettingsUiState {
                form: SettingsForm::load(),
                yt_dlp,
                deno,
                tool_tx,
                tool_rx,
                chrome_profiles: load_chrome_profiles(),
                action_tx,
                result_rx,
                index_update_state: SettingsIndexUpdateState::Idle,
                active_index_updates: 0,
                index_update_error: None,
            })),
            settings_visible: Arc::new(AtomicBool::new(false)),
            initial_setup_visible: Arc::new(AtomicBool::new(needs_initial_setup)),
            action_rx,
            result_tx,
        };
        if let Ok(mut settings) = handle.state.lock() {
            settings.refresh_all_tools();
        }
        handle
    }

    pub fn open_settings(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.open_settings();
        }
        self.settings_visible.store(true, Ordering::Release);
    }

    pub fn open_initial_setup(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.refresh_all_tools();
        }
        self.initial_setup_visible.store(true, Ordering::Release);
    }

    pub fn try_recv_action(&self) -> Result<SettingsAction, mpsc::TryRecvError> {
        self.action_rx.try_recv()
    }

    fn send_result(&self, result: SettingsResult) {
        let _ = self.result_tx.send(result);
    }

    pub(crate) fn send_index_started(&self) {
        self.send_result(SettingsResult::IndexStarted);
    }

    pub(crate) fn send_index_finished(&self, result: Result<(), String>) {
        self.send_result(SettingsResult::IndexFinished(result));
    }
}

impl SettingsUiState {
    fn open_settings(&mut self) {
        self.form = SettingsForm::load();
        self.chrome_profiles = load_chrome_profiles();
        self.refresh_all_tools();
    }

    pub fn poll_tool_updates(&mut self) {
        while let Ok(update) = self.tool_rx.try_recv() {
            match update.kind {
                ToolKind::YtDlp => self.yt_dlp = update.state,
                ToolKind::Deno => self.deno = update.state,
            }
        }
    }

    fn poll_results(&mut self) {
        while let Ok(result) = self.result_rx.try_recv() {
            match result {
                SettingsResult::Saved(data) => {
                    self.form.data = data.clone();
                    self.form.last_saved_data = data;
                    self.form.dirty = false;
                    self.form.error = None;
                }
                SettingsResult::Error(error) => self.form.error = Some(error),
                SettingsResult::Reindexed(Ok(0)) => {
                    self.form.error = None;
                    self.index_update_state = SettingsIndexUpdateState::Succeeded(Instant::now());
                }
                SettingsResult::Reindexed(Ok(_)) => self.form.error = None,
                SettingsResult::Reindexed(Err(error)) => {
                    self.form.error = Some(error.clone());
                    self.index_update_state = SettingsIndexUpdateState::Failed {
                        at: Instant::now(),
                        message: error,
                    };
                }
                SettingsResult::IndexStarted => {
                    if self.active_index_updates == 0 {
                        self.index_update_error = None;
                    }
                    self.active_index_updates = self.active_index_updates.saturating_add(1);
                    self.index_update_state = SettingsIndexUpdateState::Updating;
                }
                SettingsResult::IndexFinished(result) => {
                    self.active_index_updates = self.active_index_updates.saturating_sub(1);
                    if let Err(error) = result {
                        self.index_update_error = Some(error);
                    }
                    if self.active_index_updates == 0 {
                        self.index_update_state = match self.index_update_error.take() {
                            Some(message) => SettingsIndexUpdateState::Failed {
                                at: Instant::now(),
                                message,
                            },
                            None => SettingsIndexUpdateState::Succeeded(Instant::now()),
                        };
                    }
                }
            }
        }
    }

    fn refresh_all_tools(&mut self) {
        self.refresh_tool(ToolKind::YtDlp);
        self.refresh_tool(ToolKind::Deno);
    }

    fn refresh_tool(&mut self, kind: ToolKind) {
        match kind {
            ToolKind::YtDlp => {
                self.yt_dlp.busy = true;
                self.yt_dlp.status = "yt-dlpの状態を確認中...".to_string();
            }
            ToolKind::Deno => {
                self.deno.busy = true;
                self.deno.status = "Denoの状態を確認中...".to_string();
            }
        }
        let tx = self.tool_tx.clone();
        thread::spawn(move || {
            let state = ToolState::check(kind);
            let _ = tx.send(ToolUpdate { kind, state });
        });
    }

    fn start_tool_action(&mut self, kind: ToolKind, action: ToolAction) {
        match kind {
            ToolKind::YtDlp => {
                self.yt_dlp.busy = true;
                self.yt_dlp.status = action.status_text("yt-dlp");
            }
            ToolKind::Deno => {
                self.deno.busy = true;
                self.deno.status = action.status_text("Deno");
            }
        }

        let tx = self.tool_tx.clone();
        thread::spawn(move || {
            let result = match (kind, action) {
                (ToolKind::YtDlp, ToolAction::Install) => ensure_yt_dlp(None),
                (ToolKind::YtDlp, ToolAction::Update) => update_yt_dlp(None),
                (ToolKind::Deno, ToolAction::Install) => ensure_deno(None),
                (ToolKind::Deno, ToolAction::Update) => update_deno(None),
            };

            let mut state = ToolState::check(kind);
            if let Err(err) = result {
                state.status = format!("セットアップに失敗しました: {err}");
            }
            let _ = tx.send(ToolUpdate { kind, state });
        });
    }
}

#[derive(Clone, Copy, Debug)]
enum ToolAction {
    Install,
    Update,
}

impl ToolAction {
    fn status_text(self, label: &str) -> String {
        match self {
            ToolAction::Install => format!("{label}をセットアップ中..."),
            ToolAction::Update => format!("{label}を更新中..."),
        }
    }

    fn button_text(self) -> &'static str {
        match self {
            ToolAction::Install => "自動セットアップ",
            ToolAction::Update => "最新を取得",
        }
    }
}

impl ToolState {
    fn from_disk(kind: ToolKind) -> Self {
        let path = tool_path(kind);
        let available = path.exists() && is_executable(&path);
        let (version, status) = if available {
            ("確認中...".to_string(), "バージョンを確認中...".to_string())
        } else {
            ("未インストール".to_string(), "未インストール".to_string())
        };
        Self {
            version,
            status,
            busy: false,
            available,
        }
    }

    fn check(kind: ToolKind) -> Self {
        let path = tool_path(kind);
        if !path.exists() {
            return Self {
                version: "未インストール".to_string(),
                status: "未インストール".to_string(),
                busy: false,
                available: false,
            };
        }
        if !is_executable(&path) {
            return Self {
                version: "権限不足".to_string(),
                status: "実行権限がありません。".to_string(),
                busy: false,
                available: false,
            };
        }

        let version = read_tool_version(kind, &path).unwrap_or_else(|_| "不明".to_string());
        let status = if version == "不明" {
            "バージョン取得に失敗しました。".to_string()
        } else {
            "準備完了".to_string()
        };
        Self {
            version,
            status,
            busy: false,
            available: true,
        }
    }
}

pub fn render_toolbar(
    // 設定ウィンドウを開くためのアプリ状態
    app: &mut DownloaderApp,
    // キー入力検知に使うeguiコンテキスト
    ctx: &egui::Context,
) {
    if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Comma)) {
        app.settings_ui.open_settings();
    }
    if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::L)) {
        if let Ok(mut state) = app.log_ui.lock() {
            state.open_logs();
        }
    }
}

pub fn render_windows(
    handle: &SettingsUiHandle,
    // ビューポート描画の起点となるコンテキスト
    ctx: &egui::Context,
) {
    render_initial_setup_viewport(handle, ctx);
    render_settings_viewport(handle, ctx);
}

fn render_initial_setup_viewport(
    handle: &SettingsUiHandle,
    // ビューポート表示に使うコンテキスト
    ctx: &egui::Context,
) {
    if !handle.initial_setup_visible.load(Ordering::Acquire) {
        return;
    }

    let viewport_id = initial_setup_viewport_id();
    let builder = egui::ViewportBuilder::default()
        .with_title("初回セットアップ")
        .with_inner_size(egui::vec2(560.0, 520.0))
        .with_resizable(false)
        .with_always_on_top();

    let state = Arc::clone(&handle.state);
    let initial_setup_visible = Arc::clone(&handle.initial_setup_visible);
    let settings_visible = Arc::clone(&handle.settings_visible);
    ctx.show_viewport_deferred(viewport_id, builder, move |ui, _class| {
        paint_viewport_background(ui);
        let Ok(mut state) = state.lock() else { return };
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            initial_setup_visible.store(false, Ordering::Release);
            return;
        }
        state.poll_tool_updates();
        state.poll_results();
        if render_initial_setup_contents(ui, &mut state) {
            settings_visible.store(true, Ordering::Release);
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
    });
}

fn render_settings_viewport(handle: &SettingsUiHandle, ctx: &egui::Context) {
    if !handle.settings_visible.load(Ordering::Acquire) {
        return;
    }

    let viewport_id = settings_viewport_id();
    let builder = egui::ViewportBuilder::default()
        .with_title("設定")
        .with_inner_size(egui::vec2(640.0, 640.0))
        .with_resizable(false)
        .with_always_on_top();

    let state = Arc::clone(&handle.state);
    let settings_visible = Arc::clone(&handle.settings_visible);
    ctx.show_viewport_deferred(viewport_id, builder, move |ui, _class| {
        paint_viewport_background(ui);
        let Ok(mut state) = state.lock() else { return };
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            settings_visible.store(false, Ordering::Release);
            return;
        }
        state.poll_tool_updates();
        state.poll_results();
        render_settings_contents(ui, &mut state);
    });
}

fn render_initial_setup_contents(
    // 初回セットアップ画面の描画先
    ui: &mut egui::Ui,
    state: &mut SettingsUiState,
) -> bool {
    let mut opened_settings = false;
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 12,
            bottom: 18,
        })
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("yt-dlpのセットアップ")
                    .size(18.0)
                    .strong()
                    .color(egui::Color32::from_rgb(220, 230, 245)),
            );
            ui.label(
                egui::RichText::new(
                    "初回起動ではyt-dlpのダウンロードと実行権限の付与が必要です。\nボタン一つで最新を取得して、すぐにダウンロードを開始できます。",
                )
                .size(12.0)
                .color(egui::Color32::from_rgb(140, 150, 170)),
            );
            ui.add_space(12.0);

            render_tool_card(
                ui,
                state,
                ToolKind::YtDlp,
                ToolAction::Install,
            );
            ui.add_space(8.0);
            render_tool_card(
                ui,
                state,
                ToolKind::Deno,
                ToolAction::Install,
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let open_btn = egui::Button::new(
                        egui::RichText::new("設定を開く")
                            .size(11.5)
                            .color(egui::Color32::from_rgb(180, 200, 220)),
                    )
                    .fill(egui::Color32::from_rgb(26, 34, 52));
                    if pointing(ui.add(open_btn)).clicked() {
                        state.open_settings();
                        opened_settings = true;
                    }
                });
            });
        });

    opened_settings
}

fn render_settings_contents(
    // 設定画面の描画先
    ui: &mut egui::Ui,
    state: &mut SettingsUiState,
) {
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 12,
            bottom: 18,
        })
        .show(ui, |ui| {
            let mut style = ui.style().as_ref().clone();
            style.spacing.scroll = egui::style::ScrollStyle::thin();
            style.spacing.scroll.bar_outer_margin = 0.0;
            style.spacing.scroll.floating_allocated_width = style.spacing.scroll.bar_width;
            ui.set_style(style);

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("アプリ設定")
                            .size(18.0)
                            .strong()
                            .color(egui::Color32::from_rgb(220, 230, 245)),
                    );
                    ui.label(
                        egui::RichText::new(
                            "ウィンドウサイズ、保存先、検索対象、依存ツールの状態をまとめて管理します。",
                        )
                        .size(12.0)
                        .color(egui::Color32::from_rgb(140, 150, 170)),
                    );
                    ui.add_space(10.0);

                    let mut settings_changed =
                        render_window_section(ui, state);
                    ui.add_space(10.0);
                    settings_changed |= render_download_mode_section(ui, state);
                    ui.add_space(10.0);
                    settings_changed |= render_cookie_section(ui, state);
                    ui.add_space(10.0);
                    let (request_reindex, search_roots_changed) =
                        render_search_roots_section(ui, state);
                    settings_changed |= search_roots_changed;
                    if settings_changed {
                        state.form.mark_changed();
                    }
                    if request_reindex {
                        let _ = state.action_tx.send(SettingsAction::Reindex);
                        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                    }

                    ui.add_space(12.0);
                    render_tool_card(
                        ui,
                        state,
                        ToolKind::YtDlp,
                        ToolAction::Update,
                    );
                    ui.add_space(8.0);
                    render_tool_card(ui, state, ToolKind::Deno, ToolAction::Update);

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let save = egui::Button::new("設定を保存")
                            .fill(egui::Color32::from_rgb(16, 190, 255));
                        if pointing(ui.add_enabled(state.form.dirty, save)).clicked() {
                            let _ = state
                                .action_tx
                                .send(SettingsAction::Save(state.form.data.clone()));
                            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                        }
                        if state.form.dirty {
                            ui.label("未保存の変更があります");
                        }
                    });

                    if let Some(err) = &state.form.error {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(err)
                                .size(12.0)
                                .color(egui::Color32::from_rgb(248, 113, 113)),
                        );
                    }

                    ui.add_space(4.0);
                });
        });
}

fn initial_setup_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("initial_setup_viewport")
}

fn settings_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("settings_viewport")
}

// 設定画面のセクション共通のパネル枠。仕様どおり横幅を画面いっぱいに揃える。
fn section_frame(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(egui::Color32::from_rgb(20, 26, 40))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 56, 78)))
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add_contents(ui);
        });
}

fn render_window_section(
    // ウィンドウ設定セクションの描画先
    ui: &mut egui::Ui,
    // 入力フォーム状態を保持する設定UI
    state: &mut SettingsUiState,
) -> bool {
    let mut changed = false;

    section_frame(ui, |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(14.0, 12.0);
        egui::Grid::new("settings-grid")
            .num_columns(2)
            .spacing(egui::vec2(16.0, 12.0))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("画面幅")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(150, 160, 180)),
                );
                changed |= add_text_input(ui, &mut state.form.data.window_width, 120.0, "例: 320")
                    .changed();
                ui.end_row();

                ui.label(
                    egui::RichText::new("画面高さ")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(150, 160, 180)),
                );
                changed |=
                    add_text_input(ui, &mut state.form.data.window_height, 120.0, "例: 1000")
                        .changed();
                ui.end_row();

                ui.label(
                    egui::RichText::new("出力先フォルダ")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(150, 160, 180)),
                );
                let mut selected_dir = None;
                ui.horizontal(|ui| {
                    let input_width = (ui.available_width() - 120.0).max(200.0);
                    let default_hint_path = default_download_dir();
                    let default_hint = default_hint_path.to_string_lossy();
                    changed |= add_text_input(
                        ui,
                        &mut state.form.data.download_dir,
                        input_width,
                        default_hint.as_ref(),
                    )
                    .changed();
                    let pick_btn = egui::Button::new(
                        egui::RichText::new("フォルダを選択")
                            .size(11.5)
                            .color(egui::Color32::from_rgb(180, 200, 220)),
                    )
                    .fill(egui::Color32::from_rgb(26, 34, 52));
                    if pointing(ui.add(pick_btn)).clicked() {
                        let current = state.form.data.download_dir.trim();
                        let current_path = if current.is_empty() {
                            None
                        } else {
                            Some(PathBuf::from(current))
                        };
                        selected_dir = mac_file_dialog::choose_directory(current_path.as_deref());
                    }
                });
                if let Some(path) = selected_dir {
                    let selected = path.to_string_lossy().to_string();
                    if state.form.data.download_dir != selected {
                        state.form.data.download_dir = selected;
                        changed = true;
                    }
                }
                ui.end_row();
            });
    });

    changed
}

fn render_download_mode_section(
    // ダウンロード仕様セクションの描画先
    ui: &mut egui::Ui,
    // 選択中のダウンロード仕様を保持する設定UI
    state: &mut SettingsUiState,
) -> bool {
    let mut changed = false;

    section_frame(ui, |ui| {
        ui.label(
            egui::RichText::new("ダウンロード仕様")
                .size(13.0)
                .color(egui::Color32::from_rgb(200, 210, 230)),
        );
        ui.label(
                egui::RichText::new(
                    "画質と変換の方針を選びます。各項目にカーソルを合わせると詳細を表示します。次回のダウンロードから適用されます。",
                )
                .size(11.5)
                .color(egui::Color32::from_rgb(140, 150, 170)),
            );
        ui.add_space(6.0);

        for mode in DownloadMode::ALL {
            // 説明は画面を煩雑にしないため、ホバー時の注意書きとして表示する。
            changed |= pointing(ui.radio_value(
                &mut state.form.data.download_mode,
                mode,
                egui::RichText::new(mode.label()).size(12.5),
            ))
            .on_hover_ui(|ui| render_download_mode_hint(ui, mode))
            .changed();
            ui.add_space(2.0);
        }
    });

    changed
}

// ダウンロード仕様の詳細と注意点をホバー時の吹き出しへ描画する。
fn render_download_mode_hint(ui: &mut egui::Ui, mode: DownloadMode) {
    ui.set_max_width(300.0);
    ui.label(
        egui::RichText::new(mode.label())
            .size(12.0)
            .strong()
            .color(egui::Color32::from_rgb(220, 230, 245)),
    );
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(mode.description())
            .size(11.5)
            .color(egui::Color32::from_rgb(180, 190, 210)),
    );
    if let Some(caution) = mode.caution() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("注意: {caution}"))
                .size(11.0)
                .color(egui::Color32::from_rgb(251, 191, 36)),
        );
    }
}

fn render_cookie_section(
    // Cookie設定セクションの描画先
    ui: &mut egui::Ui,
    // Cookie関連の入力フォーム状態
    state: &mut SettingsUiState,
) -> bool {
    let mut changed = false;

    section_frame(ui, |ui| {
        ui.label(
            egui::RichText::new("YouTube認証")
                .size(13.0)
                .color(egui::Color32::from_rgb(200, 210, 230)),
        );
        ui.label(
                egui::RichText::new(
                    "bot確認が出る場合のみ有効化してください。ブラウザ名とプロファイルはyt-dlpの--cookies-from-browserに渡されます。",
                )
                .size(11.5)
                .color(egui::Color32::from_rgb(140, 150, 170)),
            );
        ui.add_space(6.0);
        changed |= pointing(ui.checkbox(
            &mut state.form.data.cookies_enabled,
            "ブラウザのクッキーを使う（bot確認対策）",
        ))
        .changed();
        ui.add_space(6.0);

        egui::Grid::new("cookies-grid")
            .num_columns(2)
            .spacing(egui::vec2(16.0, 12.0))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("ブラウザ名")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(150, 160, 180)),
                );
                let browser_hint = "例: chrome / firefox / safari";
                let browser_enabled = state.form.data.cookies_enabled;
                changed |= ui
                    .add_enabled_ui(browser_enabled, |ui| {
                        add_text_input(
                            ui,
                            &mut state.form.data.cookies_browser,
                            220.0,
                            browser_hint,
                        )
                        .changed()
                    })
                    .inner;
                ui.end_row();

                ui.label(
                    egui::RichText::new("プロファイル")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(150, 160, 180)),
                );
                let profile_hint = "例: Default / Profile 1";
                let profile_enabled = state.form.data.cookies_enabled;
                changed |= ui
                    .add_enabled_ui(profile_enabled, |ui| {
                        render_profile_input(ui, state, 220.0, profile_hint)
                    })
                    .inner;
                ui.end_row();
            });
    });

    changed
}

fn render_profile_input(
    ui: &mut egui::Ui,
    state: &mut SettingsUiState,
    width: f32,
    hint: &str,
) -> bool {
    if state.chrome_profiles.is_empty() || !is_chrome_browser(&state.form.data.cookies_browser) {
        return add_text_input(ui, &mut state.form.data.cookies_profile, width, hint).changed();
    }

    let selected_text = selected_chrome_profile_label(
        &state.form.data.cookies_profile,
        &state.chrome_profiles,
        "指定なし",
    );
    let mut changed = false;
    egui::ComboBox::from_id_salt("chrome-profile-combo")
        .width(width)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            changed |= ui
                .selectable_value(
                    &mut state.form.data.cookies_profile,
                    String::new(),
                    "指定なし",
                )
                .changed();
            for profile in &state.chrome_profiles {
                let label = chrome_profile_label(profile);
                changed |= ui
                    .selectable_value(
                        &mut state.form.data.cookies_profile,
                        profile.id.clone(),
                        label,
                    )
                    .changed();
            }
            let current = state.form.data.cookies_profile.trim().to_string();
            if !current.is_empty()
                && !state
                    .chrome_profiles
                    .iter()
                    .any(|profile| profile.id == current)
            {
                changed |= ui
                    .selectable_value(
                        &mut state.form.data.cookies_profile,
                        current.clone(),
                        format!("{current}（現在の設定）"),
                    )
                    .changed();
            }
        });
    changed
}

fn is_chrome_browser(browser: &str) -> bool {
    let browser = browser.trim();
    browser.is_empty()
        || browser.eq_ignore_ascii_case("chrome")
        || browser.eq_ignore_ascii_case("google-chrome")
        || browser.eq_ignore_ascii_case("google chrome")
}

fn selected_chrome_profile_label(
    selected: &str,
    profiles: &[ChromeProfile],
    empty_label: &str,
) -> String {
    let selected = selected.trim();
    if selected.is_empty() {
        return empty_label.to_string();
    }
    profiles
        .iter()
        .find(|profile| profile.id == selected)
        .map(chrome_profile_label)
        .unwrap_or_else(|| format!("{selected}（現在の設定）"))
}

fn chrome_profile_label(profile: &ChromeProfile) -> String {
    if profile.display_name == profile.id {
        profile.id.clone()
    } else {
        format!("{} ({})", profile.display_name, profile.id)
    }
}

fn render_search_roots_section(ui: &mut egui::Ui, state: &mut SettingsUiState) -> (bool, bool) {
    let mut should_reindex = false;
    let mut remove_index = None;
    let mut add_directory = None;
    let mut changed = false;
    let indexing = matches!(state.index_update_state, SettingsIndexUpdateState::Updating);

    section_frame(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("検索対象フォルダ")
                    .size(13.0)
                    .color(egui::Color32::from_rgb(200, 210, 230)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let btn = egui::Button::new(
                    egui::RichText::new("全体を再インデックス")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(8, 14, 24)),
                )
                .fill(egui::Color32::from_rgb(16, 190, 255));
                if pointing(ui.add_enabled(!indexing, btn)).clicked() {
                    should_reindex = true;
                }
            });
        });
        ui.label(
            egui::RichText::new("動画検索対象のルートフォルダを複数指定できます。")
                .size(11.5)
                .color(egui::Color32::from_rgb(140, 150, 170)),
        );
        ui.add_space(8.0);

        let btn = egui::Button::new(
            egui::RichText::new("フォルダを追加")
                .size(11.5)
                .color(egui::Color32::from_rgb(180, 200, 220)),
        )
        .fill(egui::Color32::from_rgb(26, 34, 52));
        if pointing(ui.add(btn)).clicked() {
            let current = state.form.data.search_roots.last().map(PathBuf::from);
            add_directory = mac_file_dialog::choose_directory(current.as_deref());
        }

        ui.add_space(6.0);
        if state.form.data.search_roots.is_empty() {
            ui.label(
                egui::RichText::new("検索対象フォルダが未設定です。")
                    .size(11.5)
                    .color(egui::Color32::from_rgb(120, 130, 150)),
            );
        } else {
            for (index, root) in state.form.data.search_roots.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(root)
                            .size(11.5)
                            .color(egui::Color32::from_rgb(170, 180, 200)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let remove_btn = egui::Button::new(
                            egui::RichText::new("削除")
                                .size(10.5)
                                .color(egui::Color32::from_rgb(248, 113, 113)),
                        )
                        .fill(egui::Color32::from_rgb(45, 26, 34));
                        if pointing(ui.add(remove_btn)).clicked() {
                            remove_index = Some(index);
                        }
                    });
                });
            }
        }

        render_index_update_status(ui, state);
    });

    if let Some(path) = add_directory {
        let value = path.to_string_lossy().to_string();
        if !state
            .form
            .data
            .search_roots
            .iter()
            .any(|existing| existing == &value)
        {
            state.form.data.search_roots.push(value);
            changed = true;
        }
    }

    if let Some(index) = remove_index {
        if index < state.form.data.search_roots.len() {
            state.form.data.search_roots.remove(index);
            changed = true;
        }
    }

    (should_reindex, changed)
}

fn render_index_update_status(ui: &mut egui::Ui, state: &mut SettingsUiState) {
    const VISIBLE_FOR: f32 = 3.2;
    let (text, color, progress, updating) = match &state.index_update_state {
        SettingsIndexUpdateState::Idle => return,
        SettingsIndexUpdateState::Updating => (
            "インデックスを更新中…".to_string(),
            egui::Color32::from_rgb(16, 190, 255),
            0.0,
            true,
        ),
        SettingsIndexUpdateState::Succeeded(at) => {
            let elapsed = at.elapsed().as_secs_f32();
            if elapsed >= VISIBLE_FOR {
                state.index_update_state = SettingsIndexUpdateState::Idle;
                return;
            }
            (
                "インデックスを更新しました".to_string(),
                egui::Color32::from_rgb(52, 211, 153),
                (elapsed / 0.45).clamp(0.0, 1.0),
                false,
            )
        }
        SettingsIndexUpdateState::Failed { at, message } => {
            let elapsed = at.elapsed().as_secs_f32();
            if elapsed >= VISIBLE_FOR + 2.0 {
                state.index_update_state = SettingsIndexUpdateState::Idle;
                return;
            }
            (
                format!("インデックスの更新に失敗しました: {message}"),
                egui::Color32::from_rgb(248, 113, 113),
                (elapsed / 0.3).clamp(0.0, 1.0),
                false,
            )
        }
    };

    ui.add_space(10.0);
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.08))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.55)))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                if updating {
                    ui.add(egui::Spinner::new().size(18.0).color(color));
                } else {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                    let center = rect.center();
                    ui.painter().circle_stroke(
                        center,
                        8.0 * settings_ease_out_back(progress),
                        egui::Stroke::new(1.8, color),
                    );
                    if matches!(
                        state.index_update_state,
                        SettingsIndexUpdateState::Succeeded(_)
                    ) {
                        paint_settings_check_mark(ui.painter(), center, color, progress);
                    } else {
                        ui.painter().line_segment(
                            [
                                center + egui::vec2(0.0, -3.5),
                                center + egui::vec2(0.0, 2.0),
                            ],
                            egui::Stroke::new(1.8, color),
                        );
                        ui.painter()
                            .circle_filled(center + egui::vec2(0.0, 4.8), 1.1, color);
                    }
                }
                ui.label(
                    egui::RichText::new(text)
                        .size(11.5)
                        .strong()
                        .color(egui::Color32::from_rgb(210, 222, 238)),
                );
            });
        });
    ui.ctx().request_repaint_after(Duration::from_millis(16));
}

fn settings_ease_out_back(value: f32) -> f32 {
    let x = value - 1.0;
    1.0 + 2.70158 * x * x * x + 1.70158 * x * x
}

fn paint_settings_check_mark(
    painter: &egui::Painter,
    center: egui::Pos2,
    color: egui::Color32,
    progress: f32,
) {
    let progress = ((progress - 0.35) / 0.65).clamp(0.0, 1.0);
    let start = center + egui::vec2(-4.0, 0.0);
    let middle = center + egui::vec2(-1.0, 3.0);
    let end = center + egui::vec2(4.5, -3.0);
    let stroke = egui::Stroke::new(1.8, color);
    if progress <= 0.4 {
        painter.line_segment([start, start.lerp(middle, progress / 0.4)], stroke);
    } else {
        painter.line_segment([start, middle], stroke);
        painter.line_segment([middle, middle.lerp(end, (progress - 0.4) / 0.6)], stroke);
    }
}

fn render_tool_card(
    // ツールカードの描画先
    ui: &mut egui::Ui,
    // ツール状態とアクションを持つ設定UI
    state: &mut SettingsUiState,
    // 表示対象のツール種別
    kind: ToolKind,
    // 表示するボタンのアクション種別
    action: ToolAction,
) {
    let panel_fill = egui::Color32::from_rgb(20, 26, 40);
    let panel_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 56, 78));

    egui::Frame::NONE
        .fill(panel_fill)
        .stroke(panel_stroke)
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            let (version, status, busy, available) = match kind {
                ToolKind::YtDlp => (
                    state.yt_dlp.version.clone(),
                    state.yt_dlp.status.clone(),
                    state.yt_dlp.busy,
                    state.yt_dlp.available,
                ),
                ToolKind::Deno => (
                    state.deno.version.clone(),
                    state.deno.status.clone(),
                    state.deno.busy,
                    state.deno.available,
                ),
            };
            let name = match kind {
                ToolKind::YtDlp => "yt-dlp",
                ToolKind::Deno => "Deno",
            };

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(name)
                        .size(14.0)
                        .color(egui::Color32::from_rgb(210, 220, 240))
                        .strong(),
                );
                if busy {
                    ui.add(egui::Spinner::new().size(16.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let btn = egui::Button::new(
                        egui::RichText::new(action.button_text())
                            .size(11.5)
                            .color(egui::Color32::from_rgb(8, 14, 24)),
                    )
                    .fill(egui::Color32::from_rgb(16, 190, 255));
                    if pointing(ui.add_enabled(!busy, btn)).clicked() {
                        state.start_tool_action(kind, action);
                    }
                });
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("バージョン: {}", version))
                        .size(12.0)
                        .color(egui::Color32::from_rgb(160, 170, 190)),
                );
                if !available {
                    ui.label(
                        egui::RichText::new("必須")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(248, 113, 113)),
                    );
                }
            });
            ui.label(
                egui::RichText::new(status)
                    .size(12.0)
                    .color(egui::Color32::from_rgb(140, 150, 170)),
            );
        });
}

fn add_text_input(
    // 入力欄を配置する描画先
    ui: &mut egui::Ui,
    // 入力内容をバインドする文字列
    text: &mut String,
    // 入力欄の横幅
    width: f32,
    // 未入力時に表示するヒント
    hint: &str,
) -> egui::Response {
    let mut style = ui.style().as_ref().clone();
    // 入力欄の背景色はここで指定しています（text_edit_bg_color / bg_fill）
    let input_bg = egui::Color32::from_rgb(32, 46, 76);
    // TextEdit専用の背景色
    style.visuals.text_edit_bg_color = Some(input_bg);
    // 非アクティブ時の背景色
    style.visuals.widgets.inactive.bg_fill = input_bg;
    // 非アクティブ時の枠線
    style.visuals.widgets.inactive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(96, 126, 170));
    // ホバー時の背景色
    style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(66, 88, 132);
    // ホバー時の枠線
    style.visuals.widgets.hovered.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(124, 158, 206));
    // アクティブ（フォーカス）時の背景色
    style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(66, 88, 132);
    // アクティブ（フォーカス）時の枠線
    style.visuals.widgets.active.fg_stroke =
        egui::Stroke::new(1.5, egui::Color32::from_rgb(90, 196, 255));
    // 非アクティブ時の角丸
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(12);
    // ホバー時の角丸
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(12);
    // アクティブ時の角丸
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(12);
    // 入力欄の高さ
    let input_height = 36.0;

    ui.scope(|ui| {
        ui.set_style(style);
        ui.add_sized(
            [width, input_height],
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .vertical_align(egui::Align::Center)
                .text_color(egui::Color32::WHITE)
                .background_color(input_bg),
        )
    })
    .inner
}

struct AppliedSettings {
    data: SettingsData,
    download_dir: PathBuf,
    window_size: egui::Vec2,
}

fn apply_settings_changes(mut data: SettingsData) -> Result<AppliedSettings, String> {
    let width = parse_dimension_input(&data.window_width)
        .ok_or_else(|| "画面の幅/高さは数値で入力してください。".to_string())?;
    let height = parse_dimension_input(&data.window_height)
        .ok_or_else(|| "画面の幅/高さは数値で入力してください。".to_string())?;
    let width = width.max(320.0);
    let height = height.max(320.0);
    let dir_input = data.download_dir.trim();
    let actual_dir = if dir_input.is_empty() {
        default_download_dir()
    } else {
        make_absolute_path(dir_input)
    };

    if data.cookies_enabled && data.cookies_browser.trim().is_empty() {
        return Err("ブラウザ名を入力してください。".to_string());
    }

    if let Err(err) = std::fs::create_dir_all(&actual_dir) {
        return Err(format!("フォルダを作成できませんでした: {err}"));
    }

    data.window_width = format_dimension(width);
    data.window_height = format_dimension(height);
    data.download_dir = actual_dir.to_string_lossy().to_string();
    data.search_roots = normalize_search_roots(&data.search_roots)?;
    save_settings(&data)?;

    Ok(AppliedSettings {
        data,
        download_dir: actual_dir,
        window_size: egui::vec2(width, height),
    })
}

pub fn process_requests(app: &mut DownloaderApp, ctx: &egui::Context) {
    while let Ok(action) = app.settings_ui.try_recv_action() {
        match action {
            SettingsAction::Reindex => {
                let result = app.request_reindex_all();
                app.settings_ui
                    .send_result(SettingsResult::Reindexed(result));
                ctx.request_repaint_of(settings_viewport_id());
            }
            SettingsAction::Save(data) => {
                let previous_roots = SettingsData::load().search_roots;
                let applied = match apply_settings_changes(data) {
                    Ok(applied) => applied,
                    Err(error) => {
                        app.settings_ui.send_result(SettingsResult::Error(error));
                        ctx.request_repaint_of(settings_viewport_id());
                        continue;
                    }
                };

                let roots = applied.data.search_roots.clone();
                if roots != previous_roots {
                    if let Err(error) = app.sync_search_roots(&roots) {
                        app.settings_ui.send_result(SettingsResult::Error(format!(
                            "検索対象フォルダの同期に失敗しました: {error}"
                        )));
                        ctx.request_repaint_of(settings_viewport_id());
                        continue;
                    }
                    app.mark_search_dirty();
                }

                app.download_dir = applied.download_dir;
                app.converter_ui.set_output_dir(app.download_dir.clone());
                app.refresh_needed = true;
                app.pending_window_resize = Some(applied.window_size);
                app.cookie_args = cookie_args_from_settings(&applied.data);
                app.download_mode = applied.data.download_mode;
                app.settings_ui
                    .send_result(SettingsResult::Saved(applied.data));
                ctx.request_repaint_of(settings_viewport_id());
            }
        }
    }
}

fn normalize_search_roots(roots: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for root in roots {
        let trimmed = root.trim();
        if trimmed.is_empty() {
            continue;
        }
        let absolute = make_absolute_path(trimmed);
        let normalized = absolute.to_string_lossy().to_string();
        if !out.iter().any(|existing| existing == &normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
}

fn parse_dimension_input(raw: &str) -> Option<f32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<f32>().ok()
}

fn format_dimension(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{:.0}", value)
    } else {
        format!("{value}")
    }
}

fn tool_path(kind: ToolKind) -> PathBuf {
    match kind {
        ToolKind::YtDlp => yt_dlp_path(),
        ToolKind::Deno => deno_path(),
    }
}

fn read_tool_version(kind: ToolKind, path: &PathBuf) -> Result<String, String> {
    let mut cmd = Command::new(path);
    match kind {
        ToolKind::YtDlp => {
            cmd.arg("--version");
        }
        ToolKind::Deno => {
            cmd.arg("--version");
        }
    }
    let output = cmd.output().map_err(|err| err.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut line = stdout.lines().next().unwrap_or("").trim().to_string();
    if line.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        line = stderr.lines().next().unwrap_or("").trim().to_string();
    }
    if line.is_empty() {
        return Err("version_not_found".to_string());
    }
    Ok(line)
}
