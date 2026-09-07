// Windowsではメイン画面の右クリックメニューから各サブ画面を開く。
// 要求フラグはmacOSと共通のアプリループで回収する。

use std::sync::atomic::{AtomicBool, Ordering};

static OPEN_SETTINGS_REQUEST: AtomicBool = AtomicBool::new(false);
static OPEN_LOGS_REQUEST: AtomicBool = AtomicBool::new(false);
static OPEN_SPEED_TEST_REQUEST: AtomicBool = AtomicBool::new(false);
static OPEN_STREAM_REQUEST: AtomicBool = AtomicBool::new(false);
static OPEN_CONVERTER_REQUEST: AtomicBool = AtomicBool::new(false);

// ネイティブメニューを持たないため、インストール処理は存在しない。
pub fn install_settings_menu() {}

pub fn take_open_settings_request() -> bool {
    OPEN_SETTINGS_REQUEST.swap(false, Ordering::Relaxed)
}

pub fn take_open_logs_request() -> bool {
    OPEN_LOGS_REQUEST.swap(false, Ordering::Relaxed)
}

pub fn take_open_speed_test_request() -> bool {
    OPEN_SPEED_TEST_REQUEST.swap(false, Ordering::Relaxed)
}

pub fn take_open_stream_request() -> bool {
    OPEN_STREAM_REQUEST.swap(false, Ordering::Relaxed)
}

pub fn take_open_converter_request() -> bool {
    OPEN_CONVERTER_REQUEST.swap(false, Ordering::Relaxed)
}

// 右クリックメニューからの要求。

pub fn request_open_settings() {
    OPEN_SETTINGS_REQUEST.store(true, Ordering::Relaxed);
}

pub fn request_open_logs() {
    OPEN_LOGS_REQUEST.store(true, Ordering::Relaxed);
}

pub fn request_open_speed_test() {
    OPEN_SPEED_TEST_REQUEST.store(true, Ordering::Relaxed);
}

pub fn request_open_stream() {
    OPEN_STREAM_REQUEST.store(true, Ordering::Relaxed);
}

pub fn request_open_converter() {
    OPEN_CONVERTER_REQUEST.store(true, Ordering::Relaxed);
}

/// メイン画面の右クリックから、Mac版と同じ機能を呼び出す。
pub fn render_context_menu(root_ui: &mut eframe::egui::Ui) {
    use eframe::egui;
    const MENU_TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(235, 240, 250);

    let ctx = root_ui.ctx();
    let (secondary_clicked, pointer_pos) = ctx.input(|input| {
        (
            input.pointer.secondary_clicked(),
            input.pointer.interact_pos(),
        )
    });
    let should_open = secondary_clicked
        && pointer_pos.is_some_and(|pos| {
            root_ui.max_rect().contains(pos) && ctx.layer_id_at(pos) == Some(root_ui.layer_id())
        });
    egui::Popup::new(
        egui::Id::new("windows_app_context_menu"),
        ctx.clone(),
        root_ui.max_rect(),
        root_ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .layout(egui::Layout::top_down_justified(egui::Align::Min))
    .open_memory(should_open.then_some(egui::SetOpenCommand::Bool(true)))
    .at_pointer_fixed()
    .show(|ui| {
        let items: [(&str, fn()); 5] = [
            ("設定...", request_open_settings),
            ("ログ...", request_open_logs),
            ("通信速度測定...", request_open_speed_test),
            ("ストリーム再生...", request_open_stream),
            ("動画をMP4に変換...", request_open_converter),
        ];
        for (label, request) in items {
            let label = egui::RichText::new(label).color(MENU_TEXT_COLOR);
            if ui.button(label).clicked() {
                request();
                ui.close();
            }
        }
    });
}
