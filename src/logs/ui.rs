use std::time::Duration;

use arboard::Clipboard;
use eframe::egui;

use std::sync::{Arc, Mutex};

use crate::cursor::pointing;
use crate::logs::AppLogger;
use crate::theme::paint_viewport_background;

pub struct LogUiState {
    pub show_logs: bool,
}

impl LogUiState {
    pub fn new() -> Self {
        Self { show_logs: false }
    }

    pub fn open_logs(&mut self) {
        self.show_logs = true;
    }
}

impl Default for LogUiState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn render_log_viewport(
    state: &Arc<Mutex<LogUiState>>,
    logs: &Arc<Mutex<AppLogger>>,
    ctx: &egui::Context,
) {
    if !state.lock().is_ok_and(|state| state.show_logs) {
        return;
    }

    let viewport_id = log_viewport_id();
    let builder = egui::ViewportBuilder::default()
        .with_title("ログ")
        .with_inner_size(egui::vec2(760.0, 460.0))
        .with_min_inner_size(egui::vec2(520.0, 280.0))
        .with_always_on_top();

    let state = Arc::clone(state);
    let logs = Arc::clone(logs);
    ctx.show_viewport_deferred(viewport_id, builder, move |ui, _class| {
        paint_viewport_background(ui);
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            if let Ok(mut state) = state.lock() {
                state.show_logs = false;
            }
            return;
        }
        render_log_contents(ui, &logs);
    });
}

fn render_log_contents(
    // ログ画面の描画先
    ui: &mut egui::Ui,
    logs: &Arc<Mutex<AppLogger>>,
) {
    let mut copy_clicked = false;
    let mut clear_clicked = false;
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 12,
            right: 12,
            top: 10,
            bottom: 12,
        })
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("ログ")
                    .size(14.0)
                    .strong()
                    .color(egui::Color32::from_rgb(226, 232, 240)),
            );
            ui.add_space(8.0);

            let list_height = (ui.available_height() - 42.0).max(130.0);
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10))
                .stroke(egui::Stroke::new(
                    1.0,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                ))
                .corner_radius(egui::CornerRadius::same(10))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .max_height(list_height)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            let Ok(logs) = logs.lock() else { return };
                            if logs.is_empty() {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new("ログはまだありません。")
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(148, 163, 184)),
                                );
                                return;
                            }

                            for (index, line) in logs.lines().enumerate() {
                                let fill = if index % 2 == 1 {
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6)
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                egui::Frame::NONE
                                    .fill(fill)
                                    .inner_margin(egui::Margin::symmetric(10, 8))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new(line)
                                                .monospace()
                                                .size(12.0)
                                                .color(egui::Color32::from_rgb(229, 231, 235)),
                                        );
                                    });
                            }
                        });
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("アプリを終了するとログはクリアされます。")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let clear_btn = egui::Button::new(
                        egui::RichText::new("表示をクリア")
                            .size(11.5)
                            .color(egui::Color32::from_rgb(226, 232, 240)),
                    )
                    .fill(egui::Color32::from_rgba_unmultiplied(226, 232, 240, 20))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
                    ));
                    if pointing(ui.add(clear_btn)).clicked() {
                        clear_clicked = true;
                    }

                    let copy_btn = egui::Button::new(
                        egui::RichText::new("直近10分をコピー")
                            .size(11.5)
                            .color(egui::Color32::from_rgb(226, 232, 240)),
                    )
                    .fill(egui::Color32::from_rgba_unmultiplied(226, 232, 240, 20))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
                    ));
                    if pointing(ui.add(copy_btn)).clicked() {
                        copy_clicked = true;
                    }
                });
            });
        });

    if clear_clicked {
        if let Ok(mut logs) = logs.lock() {
            logs.clear();
        }
    }

    if copy_clicked {
        let snapshot = logs
            .lock()
            .map(|logs| logs.build_recent_snapshot(Duration::from_secs(10 * 60)))
            .unwrap_or_default();
        if let Err(err) = copy_to_clipboard(&snapshot) {
            eprintln!("ログのコピーに失敗しました: {err}");
        }
    }
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|err| err.to_string())?;
    clipboard
        .set_text(text.to_string())
        .map_err(|err| err.to_string())
}

fn log_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("log_viewport")
}
