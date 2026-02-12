mod app;
mod app_logger;
mod bundled;
mod cursor;
mod download;
mod fs_utils;
mod log_ui;
mod mac_file_dialog;
mod mac_input_source;
mod mac_menu;
mod mac_window;
mod paths;
mod search_index;
mod settings;
mod settings_ui;
mod theme;
mod ui;

fn main() -> eframe::Result<()> {
    app::run()
}
