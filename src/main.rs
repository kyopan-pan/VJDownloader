// リリースビルドではコンソールウィンドウを出さない。
// デバッグビルドでは println!/eprintln! の出力を見るためコンソールを残す。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bundled;
mod converter;
mod cursor;
mod download;
mod fs_utils;
mod logs;
mod paths;
mod platform;
mod search_index;
mod settings;
mod speed_test;
mod stream;
mod theme;
mod ui;

fn main() -> eframe::Result<()> {
    app::run()
}
