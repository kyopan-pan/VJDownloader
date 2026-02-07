use eframe::egui;
use egui::emath::GuiRounding;

use crate::app::DownloaderApp;

pub fn render(app: &mut DownloaderApp, ctx: &egui::Context, frame: &eframe::Frame) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(10.0);

        let content_margin: i8 = 3;
        let panel_fill = egui::Color32::from_rgb(24, 30, 45);
        let panel_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(36, 44, 62));

        egui::Frame::NONE
            .fill(egui::Color32::from_rgb(15, 22, 36))
            .stroke(egui::Stroke::NONE)
            .corner_radius(egui::CornerRadius::same(24))
            .inner_margin(egui::Margin::symmetric(content_margin, content_margin))
            .show(ui, |ui| {
                let button = egui::Button::new(
                    egui::RichText::new("Download")
                        .size(20.0)
                        .color(egui::Color32::from_rgb(8, 14, 24)),
                )
                .fill(egui::Color32::from_rgb(16, 190, 255))
                .corner_radius(egui::CornerRadius::same(22));

                ui.add_enabled_ui(!app.download_in_progress, |ui| {
                    if ui
                        .add_sized([ui.available_width(), 64.0], button)
                        .on_disabled_hover_text("ダウンロード中です")
                        .clicked()
                    {
                        app.start_download_from_clipboard();
                    }
                });
            });

        ui.add_space(8.0);

        render_progress_panel(ui, ctx, app);

        ui.add_space(18.0);

        ui.label(
            egui::RichText::new("Downloads")
                .size(16.0)
                .color(egui::Color32::from_rgb(210, 220, 240)),
        );
        ui.label(
            egui::RichText::new("リストをドラッグしてVDMXへドロップ")
                .size(11.5)
                .color(egui::Color32::from_rgb(130, 140, 160)),
        );
        ui.add_space(8.0);

        egui::Frame::NONE
            .fill(panel_fill)
            .stroke(panel_stroke)
            .corner_radius(egui::CornerRadius::same(18))
            .inner_margin(egui::Margin::symmetric(content_margin, content_margin))
            .show(ui, |ui| {
                ui.set_min_height(360.0);
                ui.set_max_height(360.0);
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(6, 6))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(360.0)
                            .show(ui, |ui| {
                                if app.downloaded_files.is_empty() {
                                    ui.label(
                                        egui::RichText::new("まだダウンロードがありません。")
                                            .size(12.5)
                                            .color(egui::Color32::from_rgb(120, 130, 150)),
                                    );
                                } else {
                                    let files = app.downloaded_files.clone();
                                    let mut remove_paths = Vec::new();
                                    let previous_spacing = ui.spacing().item_spacing;
                                    ui.spacing_mut().item_spacing =
                                        egui::vec2(previous_spacing.x, 0.0);
                                    let font_id = egui::FontId::proportional(13.5);
                                    let text_center_offset = ui.fonts_mut(|fonts| {
                                        let galley = fonts.layout_no_wrap(
                                            "Ag".to_string(),
                                            font_id.clone(),
                                            egui::Color32::WHITE,
                                        );
                                        galley.rect.center().y - galley.mesh_bounds.center().y
                                    });
                                    for path in &files {
                                    let filename = path
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("unknown");
                                    let row_width =
                                        (ui.available_width() - ui.spacing().scroll.bar_width).max(0.0);
                                let row_height = 36.0;
                                let row_padding_x = 12.0;
                                let remove_width = 28.0;
                                let remove_height = 28.0;
                                let remove_spacing = 8.0;
                                let text_max_width = (row_width
                                    - row_padding_x * 2.0
                                    - remove_width
                                    - remove_spacing)
                                    .max(0.0);
                                let text =
                                    truncate_with_ellipsis(ui, filename, text_max_width, &font_id);

                                let (row_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(row_width, row_height),
                                    egui::Sense::hover(),
                                );
                                let row_rect =
                                    row_rect.round_to_pixels(ctx.pixels_per_point());
                                let base_fill = egui::Color32::from_rgb(24, 30, 45);
                                let hover_fill = egui::Color32::from_rgb(24, 48, 70);
                                let row_hovered = ctx.input(|i| {
                                    i.pointer
                                        .latest_pos()
                                        .map_or(false, |pos| row_rect.contains(pos))
                                });
                                let fill = if row_hovered {
                                    hover_fill
                                } else {
                                    base_fill
                                };
                                ui.painter().rect_filled(
                                    row_rect,
                                    egui::CornerRadius::same(0),
                                    fill,
                                );

                                if row_hovered {
                                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                                }

                                let inner_rect =
                                    row_rect.shrink2(egui::vec2(row_padding_x, 0.0));
                                let text_color = egui::Color32::from_rgb(220, 230, 245);
                                let text_pos = egui::pos2(
                                    inner_rect.left(),
                                    row_rect.center().y + text_center_offset,
                                );
                                ui.painter().text(
                                    text_pos,
                                    egui::Align2::LEFT_CENTER,
                                    text,
                                    font_id.clone(),
                                    text_color,
                                );

                                let remove_rect = egui::Rect::from_min_size(
                                    egui::pos2(
                                        row_rect.right() - row_padding_x - remove_width,
                                        row_rect.center().y - remove_height * 0.5,
                                    ),
                                    egui::vec2(remove_width, remove_height),
                                );
                                let remove_button = ui.put(
                                    remove_rect,
                                    egui::Button::new(
                                        egui::RichText::new("✕")
                                            .size(15.0)
                                            .color(egui::Color32::from_rgb(200, 210, 230)),
                                    )
                                    .frame(false),
                                );
                                if remove_button.clicked() {
                                    remove_paths.push(path.clone());
                                }

                                let drag_rect = {
                                    let max_x = remove_button.rect.left().min(row_rect.right());
                                    if max_x > row_rect.left() {
                                        egui::Rect::from_min_max(
                                            row_rect.min,
                                            egui::pos2(max_x, row_rect.bottom()),
                                        )
                                    } else {
                                        row_rect
                                    }
                                };
                                let drag_response = ui.interact(
                                    drag_rect,
                                    ui.make_persistent_id((path, "drag_row")),
                                    egui::Sense::drag(),
                                );
                                if drag_response.drag_started() {
                                    app.start_native_drag(frame, path);
                                }
                                    }
                                    ui.spacing_mut().item_spacing = previous_spacing;

                                    if !remove_paths.is_empty() {
                                        for path in remove_paths {
                                            app.delete_download(&path);
                                        }
                                    }
                                }
                            });
                    });
            });

        ui.add_space(12.0);

        egui::CollapsingHeader::new("Status")
            .default_open(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for line in &app.status {
                            ui.label(
                                egui::RichText::new(line)
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(130, 140, 160)),
                            );
                        }
                    });
                ui.add_space(6.0);
                if ui.small_button("ログをクリア").clicked() {
                    app.status.clear();
                    app.push_status("ログをクリアしました。");
                }
            });
    });
}

fn render_progress_panel(ui: &mut egui::Ui, ctx: &egui::Context, app: &DownloaderApp) {
    let idle = !app.progress_visible;
    let opacity = if idle { 0.6 } else { 1.0 };

    let panel_fill = apply_opacity(
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 13),
        opacity,
    );
    let panel_stroke = apply_opacity(
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
        opacity,
    );
    let label_color =
        apply_opacity(egui::Color32::from_rgb(203, 213, 225), opacity);

    egui::Frame::NONE
        .fill(panel_fill)
        .stroke(egui::Stroke::new(1.0, panel_stroke))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin {
            left: 12,
            right: 12,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            let label_text = if app.progress_message.is_empty() {
                "待機中..."
            } else {
                app.progress_message.as_str()
            };
            ui.label(
                egui::RichText::new(label_text)
                    .size(12.0)
                    .color(label_color)
                    .strong(),
            );
            ui.add_space(6.0);

            let bar_height = 12.0;
            let bar_width = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(bar_width, bar_height),
                egui::Sense::hover(),
            );

            let track_color = apply_opacity(
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 31),
                opacity,
            );
            let bar_left = apply_opacity(egui::Color32::from_rgb(56, 189, 248), opacity);
            let bar_right = apply_opacity(egui::Color32::from_rgb(14, 165, 233), opacity);
            let rounding = egui::CornerRadius::same(8);

            ui.painter().rect_filled(rect, rounding, track_color);

            if app.progress_visible {
                if app.progress_value < 0.0 {
                    let t = ctx.input(|input| input.time) as f32;
                    let speed = 0.6f32;
                    let segment_fraction = 0.28f32;
                    let phase = (t * speed) % 1.0;
                    let start = phase * (1.0 + segment_fraction) - segment_fraction;
                    let end = start + segment_fraction;
                    let start_px = rect.left() + rect.width() * start;
                    let end_px = rect.left() + rect.width() * end;
                    let seg_min = start_px.max(rect.left());
                    let seg_max = end_px.min(rect.right());
                    if seg_max > seg_min {
                        let seg_rect = egui::Rect::from_min_max(
                            egui::pos2(seg_min, rect.top()),
                            egui::pos2(seg_max, rect.bottom()),
                        );
                        paint_bar_segment(ui.painter(), seg_rect, rounding, bar_left, bar_right);
                    }
                    ctx.request_repaint();
                } else {
                    let progress = app.progress_value.clamp(0.0, 1.0);
                    if progress > 0.0 {
                        let fill_width = rect.width() * progress;
                        let fill_rect = egui::Rect::from_min_max(
                            rect.min,
                            egui::pos2(rect.left() + fill_width, rect.bottom()),
                        );
                        paint_bar_segment(ui.painter(), fill_rect, rounding, bar_left, bar_right);
                    }
                }
            }
        });
}

fn paint_bar_segment(
    painter: &egui::Painter,
    rect: egui::Rect,
    rounding: egui::CornerRadius,
    left: egui::Color32,
    right: egui::Color32,
) {
    if rect.width() <= 2.0 {
        painter.rect_filled(rect, rounding, left);
        return;
    }

    let mid_x = rect.center().x;
    let left_rect = egui::Rect::from_min_max(rect.min, egui::pos2(mid_x, rect.bottom()));
    let right_rect = egui::Rect::from_min_max(egui::pos2(mid_x, rect.top()), rect.max);

    let left_rounding = egui::CornerRadius {
        nw: rounding.nw,
        sw: rounding.sw,
        ne: 0,
        se: 0,
    };
    let right_rounding = egui::CornerRadius {
        nw: 0,
        sw: 0,
        ne: rounding.ne,
        se: rounding.se,
    };

    painter.rect_filled(left_rect, left_rounding, left);
    painter.rect_filled(right_rect, right_rounding, right);
}

fn apply_opacity(color: egui::Color32, opacity: f32) -> egui::Color32 {
    let alpha = (color.a() as f32 * opacity).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn truncate_with_ellipsis(
    ui: &egui::Ui,
    text: &str,
    max_width: f32,
    font_id: &egui::FontId,
) -> String {
    if max_width <= 0.0 {
        return "...".to_string();
    }

    let ellipsis = "...";
    let ellipsis_width = text_width(ui, ellipsis, font_id);
    if text_width(ui, text, font_id) <= max_width {
        return text.to_string();
    }
    if ellipsis_width >= max_width {
        return ellipsis.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high + 1) / 2;
        let candidate: String = chars[..mid].iter().collect();
        let width = text_width(ui, &(candidate.clone() + ellipsis), font_id);
        if width <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }

    let mut out: String = chars[..low].iter().collect();
    out.push_str(ellipsis);
    out
}

fn text_width(ui: &egui::Ui, text: &str, font_id: &egui::FontId) -> f32 {
    ui.fonts_mut(|fonts| {
        let galley = fonts.layout_no_wrap(text.to_string(), font_id.clone(), egui::Color32::WHITE);
        galley.size().x
    })
}
