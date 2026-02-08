mod app;
mod bundled;
mod download;
mod fs_utils;
mod mac_file_dialog;
mod mac_menu;
mod paths;
mod settings;
mod settings_ui;
mod theme;
mod ui;

fn main() -> eframe::Result<()> {
    app::run()
}
