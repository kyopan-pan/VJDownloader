use eframe::egui;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::app::DownloaderApp;
use crate::download::{ensure_deno, ensure_yt_dlp, update_deno, update_yt_dlp};
use crate::fs_utils::is_executable;
use crate::mac_file_dialog;
use crate::paths::{default_download_dir, deno_path, make_absolute_path, yt_dlp_path};
use crate::settings::{SettingsData, save_settings};

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
    error: Option<String>,
}

pub struct SettingsUiState {
    pub show_settings: bool,
    pub show_initial_setup: bool,
    form: SettingsForm,
    yt_dlp: ToolState,
    deno: ToolState,
    tool_tx: mpsc::Sender<ToolUpdate>,
    tool_rx: mpsc::Receiver<ToolUpdate>,
    last_auto_refresh: Instant,
}

impl SettingsUiState {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let yt_dlp = ToolState::from_disk(ToolKind::YtDlp);
        let deno = ToolState::from_disk(ToolKind::Deno);
        let mut state = Self {
            show_settings: false,
            show_initial_setup: !yt_dlp.available,
            form: SettingsForm {
                data: SettingsData::load(),
                error: None,
            },
            yt_dlp,
            deno,
            tool_tx: tx,
            tool_rx: rx,
            last_auto_refresh: Instant::now() - Duration::from_secs(10),
        };
        state.refresh_all_tools();
        state
    }

    pub fn open_settings(&mut self) {
        self.form = SettingsForm {
            data: SettingsData::load(),
            error: None,
        };
        self.show_settings = true;
        self.refresh_all_tools();
    }

    pub fn open_initial_setup(&mut self) {
        self.show_initial_setup = true;
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

    pub fn auto_refresh_if_needed(&mut self) {
        if (self.yt_dlp.available && self.deno.available) || self.yt_dlp.busy || self.deno.busy {
            return;
        }
        if self.last_auto_refresh.elapsed() >= Duration::from_secs(5) {
            self.refresh_all_tools();
            self.last_auto_refresh = Instant::now();
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
}

pub fn render_windows(
    // 表示フラグを持つアプリ状態
    app: &mut DownloaderApp,
    // ビューポート描画の起点となるコンテキスト
    ctx: &egui::Context,
) {
    render_initial_setup_viewport(app, ctx);
    render_settings_viewport(app, ctx);
}

fn render_initial_setup_viewport(
    // 初回セットアップ表示フラグと状態を持つアプリ
    app: &mut DownloaderApp,
    // ビューポート表示に使うコンテキスト
    ctx: &egui::Context,
) {
    if !app.settings_ui.show_initial_setup {
        return;
    }

    let mut close_requested = false;
    let viewport_id = initial_setup_viewport_id();
    let builder = egui::ViewportBuilder::default()
        .with_title("初回セットアップ")
        .with_inner_size(egui::vec2(560.0, 520.0))
        .with_resizable(false);

    ctx.show_viewport_immediate(viewport_id, builder, |ctx, class| {
        if ctx.input(|i| i.viewport().close_requested()) {
            close_requested = true;
        }

        match class {
            egui::ViewportClass::Embedded => {
                let mut open = true;
                egui::Window::new("初回セットアップ")
                    .collapsible(false)
                    .resizable(false)
                    .default_width(560.0)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        render_initial_setup_contents(ui, app);
                    });
                if !open {
                    close_requested = true;
                }
            }
            _ => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_initial_setup_contents(ui, app);
                });
            }
        }
    });

    if close_requested {
        app.settings_ui.show_initial_setup = false;
    }
}

fn render_settings_viewport(
    // 設定画面の表示フラグと状態を持つアプリ
    app: &mut DownloaderApp,
    // ビューポート表示に使うコンテキスト
    ctx: &egui::Context,
) {
    if !app.settings_ui.show_settings {
        return;
    }

    let mut close_requested = false;
    let viewport_id = settings_viewport_id();
    let builder = egui::ViewportBuilder::default()
        .with_title("設定")
        .with_inner_size(egui::vec2(640.0, 640.0))
        .with_resizable(false);

    ctx.show_viewport_immediate(viewport_id, builder, |ctx, class| {
        if ctx.input(|i| i.viewport().close_requested()) {
            close_requested = true;
        }

        match class {
            egui::ViewportClass::Embedded => {
                let mut open = true;
                egui::Window::new("設定")
                    .collapsible(false)
                    .resizable(false)
                    .default_width(620.0)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        render_settings_contents(ui, app, &mut close_requested);
                    });
                if !open {
                    close_requested = true;
                }
            }
            _ => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_settings_contents(ui, app, &mut close_requested);
                });
            }
        }
    });

    if close_requested {
        app.settings_ui.show_settings = false;
    }
}

fn render_initial_setup_contents(
    // 初回セットアップ画面の描画先
    ui: &mut egui::Ui,
    // ツールの状態や操作を持つアプリ
    app: &mut DownloaderApp,
) {
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
                &mut app.settings_ui,
                ToolKind::YtDlp,
                ToolAction::Install,
            );
            ui.add_space(8.0);
            render_tool_card(
                ui,
                &mut app.settings_ui,
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
                    if ui.add(open_btn).clicked() {
                        app.settings_ui.open_settings();
                    }
                });
            });
        });
}

fn render_settings_contents(
    // 設定画面の描画先
    ui: &mut egui::Ui,
    // 設定値・ツール状態を保持するアプリ
    app: &mut DownloaderApp,
    // OK/キャンセルで閉じるべきかのフラグ
    should_close: &mut bool,
) {
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 12,
            bottom: 18,
        })
        .show(ui, |ui| {
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

                    render_window_section(ui, &mut app.settings_ui);
                    ui.add_space(10.0);
                    render_cookie_section(ui, &mut app.settings_ui);
                    ui.add_space(10.0);
                    let request_reindex = render_search_roots_section(ui, &mut app.settings_ui);
                    if request_reindex {
                        if let Err(err) = app.request_reindex_all() {
                            app.settings_ui.form.error = Some(err);
                        } else {
                            app.settings_ui.form.error = None;
                        }
                    }

                    ui.add_space(12.0);
                    render_tool_card(
                        ui,
                        &mut app.settings_ui,
                        ToolKind::YtDlp,
                        ToolAction::Update,
                    );
                    ui.add_space(8.0);
                    render_tool_card(ui, &mut app.settings_ui, ToolKind::Deno, ToolAction::Update);

                    if let Some(err) = &app.settings_ui.form.error {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(err)
                                .size(12.0)
                                .color(egui::Color32::from_rgb(248, 113, 113)),
                        );
                    }

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let save_btn = egui::Button::new(
                                egui::RichText::new("OK")
                                    .size(12.5)
                                    .color(egui::Color32::from_rgb(8, 14, 24)),
                            )
                            .fill(egui::Color32::from_rgb(16, 190, 255));
                            if ui.add(save_btn).clicked() {
                                if let Err(err) = apply_settings_changes(
                                    &mut app.settings_ui,
                                    &mut app.download_dir,
                                    &mut app.refresh_needed,
                                    &mut app.pending_window_resize,
                                ) {
                                    app.settings_ui.form.error = Some(err);
                                } else {
                                    let roots = app.settings_ui.form.data.search_roots.clone();
                                    match app.sync_search_roots(&roots) {
                                        Ok(()) => {
                                            app.settings_ui.form.error = None;
                                            app.mark_search_dirty();
                                            *should_close = true;
                                        }
                                        Err(err) => {
                                            app.settings_ui.form.error = Some(format!(
                                                "検索対象フォルダの同期に失敗しました: {err}"
                                            ));
                                        }
                                    }
                                }
                            }

                            let cancel_btn = egui::Button::new(
                                egui::RichText::new("キャンセル")
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(180, 190, 210)),
                            )
                            .fill(egui::Color32::from_rgb(24, 30, 45));
                            if ui.add(cancel_btn).clicked() {
                                *should_close = true;
                                app.settings_ui.form.error = None;
                            }
                        });
                    });
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

fn render_window_section(
    // ウィンドウ設定セクションの描画先
    ui: &mut egui::Ui,
    // 入力フォーム状態を保持する設定UI
    state: &mut SettingsUiState,
) {
    let panel_fill = egui::Color32::from_rgb(20, 26, 40);
    let panel_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 56, 78));

    egui::Frame::NONE
        .fill(panel_fill)
        .stroke(panel_stroke)
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
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
                    add_text_input(ui, &mut state.form.data.window_width, 120.0, "例: 300");
                    ui.end_row();

                    ui.label(
                        egui::RichText::new("画面高さ")
                            .size(12.0)
                            .color(egui::Color32::from_rgb(150, 160, 180)),
                    );
                    add_text_input(ui, &mut state.form.data.window_height, 120.0, "例: 1000");
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
                        add_text_input(
                            ui,
                            &mut state.form.data.download_dir,
                            input_width,
                            default_hint.as_ref(),
                        );
                        let pick_btn = egui::Button::new(
                            egui::RichText::new("フォルダを選択")
                                .size(11.5)
                                .color(egui::Color32::from_rgb(180, 200, 220)),
                        )
                        .fill(egui::Color32::from_rgb(26, 34, 52));
                        if ui.add(pick_btn).clicked() {
                            let current = state.form.data.download_dir.trim();
                            let current_path = if current.is_empty() {
                                None
                            } else {
                                Some(PathBuf::from(current))
                            };
                            selected_dir =
                                mac_file_dialog::choose_directory(current_path.as_deref());
                        }
                    });
                    if let Some(path) = selected_dir {
                        state.form.data.download_dir = path.to_string_lossy().to_string();
                    }
                    ui.end_row();
                });
        });
}

fn render_cookie_section(
    // Cookie設定セクションの描画先
    ui: &mut egui::Ui,
    // Cookie関連の入力フォーム状態
    state: &mut SettingsUiState,
) {
    let panel_fill = egui::Color32::from_rgb(20, 26, 40);
    let panel_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 56, 78));

    egui::Frame::NONE
        .fill(panel_fill)
        .stroke(panel_stroke)
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
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
            ui.checkbox(
                &mut state.form.data.cookies_enabled,
                "ブラウザのクッキーを使う（bot確認対策）",
            );
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
                    ui.add_enabled_ui(browser_enabled, |ui| {
                        add_text_input(ui, &mut state.form.data.cookies_browser, 220.0, browser_hint);
                    });
                    ui.end_row();

                    ui.label(
                        egui::RichText::new("プロファイル")
                            .size(12.0)
                            .color(egui::Color32::from_rgb(150, 160, 180)),
                    );
                    let profile_hint = "例: Default / Profile 1";
                    let profile_enabled = state.form.data.cookies_enabled;
                    ui.add_enabled_ui(profile_enabled, |ui| {
                        add_text_input(ui, &mut state.form.data.cookies_profile, 220.0, profile_hint);
                    });
                    ui.end_row();
                });
        });
}

fn render_search_roots_section(ui: &mut egui::Ui, state: &mut SettingsUiState) -> bool {
    let panel_fill = egui::Color32::from_rgb(20, 26, 40);
    let panel_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 56, 78));
    let mut should_reindex = false;
    let mut remove_index = None;
    let mut add_directory = None;

    egui::Frame::NONE
        .fill(panel_fill)
        .stroke(panel_stroke)
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
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
                    if ui.add(btn).clicked() {
                        should_reindex = true;
                    }
                });
            });
            ui.label(
                egui::RichText::new("mp4検索対象のルートフォルダを複数指定できます。")
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
            if ui.add(btn).clicked() {
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
                            if ui.add(remove_btn).clicked() {
                                remove_index = Some(index);
                            }
                        });
                    });
                }
            }
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
        }
    }

    if let Some(index) = remove_index {
        if index < state.form.data.search_roots.len() {
            state.form.data.search_roots.remove(index);
        }
    }

    should_reindex
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
                    if ui.add_enabled(!busy, btn).clicked() {
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

fn apply_settings_changes(
    state: &mut SettingsUiState,
    download_dir: &mut PathBuf,
    refresh_needed: &mut bool,
    pending_resize: &mut Option<egui::Vec2>,
) -> Result<(), String> {
    let mut data = state.form.data.clone();
    let width = parse_dimension_input(&data.window_width)
        .ok_or_else(|| "画面の幅/高さは数値で入力してください。".to_string())?;
    let height = parse_dimension_input(&data.window_height)
        .ok_or_else(|| "画面の幅/高さは数値で入力してください。".to_string())?;
    let width = width.max(260.0);
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

    state.form.data = data;
    *download_dir = actual_dir;
    *refresh_needed = true;
    *pending_resize = Some(egui::vec2(width, height));
    Ok(())
}

fn normalize_search_roots(roots: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for root in roots {
        let trimmed = root.trim();
        if trimmed.is_empty() {
            continue;
        }
        let absolute = make_absolute_path(trimmed);
        if !absolute.is_dir() {
            return Err(format!(
                "検索対象フォルダがディレクトリではありません: {}",
                absolute.to_string_lossy()
            ));
        }
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
