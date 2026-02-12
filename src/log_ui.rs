use std::time::Duration;

use arboard::Clipboard;
use eframe::egui;

use crate::app::DownloaderApp;

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
    // ログウィンドウ表示状態とログ本体を保持するアプリ
    app: &mut DownloaderApp,
    // ビューポート描画に使うコンテキスト
    ctx: &egui::Context,
) {
    if !app.log_ui.show_logs {
        return;
    }

    let mut close_requested = false;
    let viewport_id = log_viewport_id();
    let builder = egui::ViewportBuilder::default()
        .with_title("ログ")
        .with_inner_size(egui::vec2(760.0, 460.0))
        .with_min_inner_size(egui::vec2(520.0, 280.0))
        .with_always_on_top();

    ctx.show_viewport_immediate(viewport_id, builder, |ctx, class| {
        if ctx.input(|i| i.viewport().close_requested()) {
            close_requested = true;
        }

        match class {
            egui::ViewportClass::Embedded => {
                let mut open = true;
                egui::Window::new("ログ")
                    .collapsible(false)
                    .resizable(true)
                    .default_width(740.0)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        render_log_contents(ui, app);
                    });
                if !open {
                    close_requested = true;
                }
            }
            _ => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_log_contents(ui, app);
                });
            }
        }
    });

    if close_requested {
        app.log_ui.show_logs = false;
    }
}

fn render_log_contents(
    // ログ画面の描画先
    ui: &mut egui::Ui,
    // ログ一覧とボタン操作を保持するアプリ
    app: &mut DownloaderApp,
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
                            if app.status_logs.is_empty() {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new("ログはまだありません。")
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(148, 163, 184)),
                                );
                                return;
                            }

                            for (index, line) in app.status_logs.lines().enumerate() {
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
                    if ui.add(clear_btn).clicked() {
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
                    if ui.add(copy_btn).clicked() {
                        copy_clicked = true;
                    }
                });
            });
        });

    if clear_clicked {
        app.clear_logs();
    }

    if copy_clicked {
        let snapshot = app.build_recent_log_snapshot(Duration::from_secs(10 * 60));
        if let Err(err) = copy_to_clipboard(&snapshot) {
            app.push_status(format!("ログのコピーに失敗しました: {err}"));
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
