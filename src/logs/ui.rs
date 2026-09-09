use arboard::Clipboard;
use eframe::egui;
use time::Duration;

use std::sync::{Arc, Mutex};

use crate::cursor::pointing;
use crate::logs::{AppLogger, Level, file};
use crate::theme::paint_viewport_background;

/// フッターのボタン文字サイズ。ボタン行の高さ計算と描画で同じ値を使う。
const FOOTER_TEXT_SIZE: f32 = 11.5;
/// クリップボードへコピーする範囲。
const COPY_WINDOW: Duration = Duration::minutes(10);

/// リストとフッターの間隔。
const FOOTER_GAP: f32 = 8.0;

/// レベルごとの文字色。エラーと警告を目で拾えるようにし、詳細ログは沈める。
fn level_color(level: Level) -> egui::Color32 {
    match level {
        Level::Error => egui::Color32::from_rgb(248, 113, 113),
        Level::Warn => egui::Color32::from_rgb(251, 191, 36),
        Level::Info => egui::Color32::from_rgb(229, 231, 235),
        Level::Debug => egui::Color32::from_rgb(148, 163, 184),
    }
}

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

            // ボタン行の高さはテーマのボタン余白とフォント高で決まる。固定値で見積もると
            // 余白を変えたときに確保量が足りず、フッターがウィンドウ下端へ張り付く。
            let button_height = ui
                .ctx()
                .fonts_mut(|fonts| fonts.row_height(&egui::FontId::proportional(FOOTER_TEXT_SIZE)))
                + ui.spacing().button_padding.y * 2.0;
            let footer_height = ui.spacing().item_spacing.y * 2.0 + FOOTER_GAP + button_height;
            let list_height = (ui.available_height() - footer_height).max(130.0);
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

                            for (index, entry) in logs.entries().enumerate() {
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
                                            egui::RichText::new(entry.display_line())
                                                .monospace()
                                                .size(12.0)
                                                .color(level_color(entry.level)),
                                        );
                                    });
                            }
                        });
                });

            ui.add_space(FOOTER_GAP);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "ログファイル: {}",
                        file::current_file_path().to_string_lossy()
                    ))
                    .size(12.0)
                    .color(egui::Color32::from_rgb(148, 163, 184)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let clear_btn = egui::Button::new(
                        egui::RichText::new("表示をクリア")
                            .size(FOOTER_TEXT_SIZE)
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
                            .size(FOOTER_TEXT_SIZE)
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

    if clear_clicked && let Ok(mut logs) = logs.lock() {
        logs.clear();
    }

    if copy_clicked {
        let snapshot = logs
            .lock()
            .map(|logs| logs.build_recent_snapshot(COPY_WINDOW))
            .unwrap_or_default();
        if let Err(err) = copy_to_clipboard(&snapshot) {
            crate::log_error!(App, "ログのコピーに失敗しました: {err}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::{LogEntry, Source};
    use crate::theme::apply_theme;

    const LEVELS: [Level; 4] = [Level::Debug, Level::Info, Level::Warn, Level::Error];

    fn test_entry(level: Level, index: usize) -> LogEntry {
        LogEntry {
            at: time::OffsetDateTime::now_utc(),
            level,
            source: Source::App,
            body: format!("テストログ {index}"),
        }
    }

    /// ログ画面の中身を描画し、下端の余白を含めて実際に使われた高さを返す。
    fn content_bottom(window_height: f32, log_lines: usize) -> f32 {
        let ctx = egui::Context::default();
        apply_theme(&ctx);

        let logs = Arc::new(Mutex::new(AppLogger::new()));
        if let Ok(mut logs) = logs.lock() {
            for index in 0..log_lines {
                logs.push(test_entry(LEVELS[index % LEVELS.len()], index));
            }
        }

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(760.0, window_height),
            )),
            ..Default::default()
        };

        let mut used_bottom = 0.0;
        // ScrollArea は前フレームの情報で高さが決まるため、2フレーム描画してから測る。
        for _ in 0..2 {
            used_bottom = 0.0;
            let _ = ctx.run_ui(input.clone(), |ui| {
                egui::Frame::NONE.show(ui, |ui| {
                    render_log_contents(ui, &logs);
                    // ui.max_rect() は内容に合わせて広がるため、ウィンドウ高と直接比べる。
                    used_bottom = ui.min_rect().bottom();
                });
            });
        }
        used_bottom
    }

    #[test]
    fn every_level_has_its_own_color() {
        // 同じ色のレベルがあると色分けの意味が無くなるため、4色すべてが異なることを確かめる。
        let mut colors = LEVELS.map(level_color).to_vec();
        colors.sort_by_key(|color| color.to_array());
        colors.dedup();
        assert_eq!(colors.len(), LEVELS.len(), "レベルの色が重複している");
    }

    #[test]
    fn display_line_aligns_columns_across_levels() {
        // 等幅フォントで桁を揃える前提なので、本文の開始位置がレベルによってずれないこと。
        let offsets = LEVELS.map(|level| {
            let line = test_entry(level, 0).display_line();
            line.find("テストログ").expect("本文が欠けている")
        });
        assert!(
            offsets.iter().all(|offset| *offset == offsets[0]),
            "レベルによって本文の開始位置がずれている: {offsets:?}"
        );
    }

    #[test]
    fn footer_keeps_margin_from_window_bottom() {
        // ボタン行の高さを固定値で見積もると下端へ張り付くため、余白が残ることを確認する。
        for (height, lines) in [(460.0, 0), (460.0, 200), (280.0, 0), (900.0, 5)] {
            let used_bottom = content_bottom(height, lines);
            assert!(
                used_bottom <= height,
                "高さ{height}・{lines}行でフッターが下端をはみ出した: 使用{used_bottom} > 高さ{height}"
            );
        }
    }
}
