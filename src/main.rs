mod app;
mod bundled;
mod download;
mod fs_utils;
mod paths;
mod settings;
mod theme;
mod ui;

fn main() -> eframe::Result<()> {
    app::run()
}
