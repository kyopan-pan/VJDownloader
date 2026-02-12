use eframe::egui;
use eframe::emath::GuiRounding;

use crate::app::DownloaderApp;
use crate::settings_ui;

pub fn render(
    // UI全体の状態とアクションの入口
    app: &mut DownloaderApp,
    // 描画・入力を統括するeguiコンテキスト
    ctx: &egui::Context,
    // ウィンドウ/フレーム操作に使うハンドル
    frame: &eframe::Frame,
) {
    settings_ui::render_toolbar(app, ctx);
    let panel_bg = egui::Color32::from_rgb(15, 23, 42);
    let panel_frame = egui::Frame::NONE
        .fill(panel_bg)
        .inner_margin(egui::Margin::symmetric(16, 16));

    egui::SidePanel::left("download_section")
        .resizable(true)
        .default_width(360.0)
        .min_width(280.0)
        .frame(panel_frame.clone())
        .show(ctx, |ui| {
            render_download_section(ui, ctx, app, frame);
        });

    egui::CentralPanel::default()
        .frame(panel_frame)
        .show(ctx, |ui| {
            render_search_section(ui, ctx, app, frame);
        });

    settings_ui::render_windows(app, ctx);
}

fn render_download_section(
    // ダウンロード画面の描画先UI
    ui: &mut egui::Ui,
    // 入力状態や再描画を扱うコンテキスト
    ctx: &egui::Context,
    // ダウンロード状態と操作を保持するアプリ状態
    app: &mut DownloaderApp,
    // ネイティブドラッグなどフレーム操作に利用
    frame: &eframe::Frame,
) {
    ui.add_space(6.0);

    let content_margin: i8 = 3;
    let panel_fill = egui::Color32::from_rgb(24, 30, 45);
    let panel_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(36, 44, 62));

    egui::Frame::NONE
        .fill(egui::Color32::from_rgb(15, 22, 36))
        .stroke(egui::Stroke::NONE)
        .corner_radius(egui::CornerRadius::same(18))
        .inner_margin(egui::Margin::symmetric(content_margin, content_margin))
        .show(ui, |ui| {
            let (label, fill) = if app.download_in_progress {
                ("Stop", egui::Color32::from_rgb(248, 113, 113))
            } else {
                ("Download", egui::Color32::from_rgb(16, 190, 255))
            };
            let button = egui::Button::new(
                egui::RichText::new(label)
                    .size(18.0)
                    .color(egui::Color32::from_rgb(8, 14, 24)),
            )
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(18));

            if ui.add_sized([ui.available_width(), 48.0], button).clicked() {
                if app.download_in_progress {
                    app.request_cancel_download();
                } else {
                    app.start_download_from_clipboard();
                }
            }
        });

    ui.add_space(8.0);
    render_progress_panel(ui, ctx, app);
    ui.add_space(16.0);

    ui.label(
        egui::RichText::new("Downloads")
            .size(13.0)
            .color(egui::Color32::from_rgb(226, 232, 240)),
    );
    ui.label(
        egui::RichText::new("リストをドラッグしてVDMXへドロップ")
            .size(11.5)
            .color(egui::Color32::from_rgb(130, 140, 160)),
    );
    ui.add_space(8.0);

    let list_height = ui.available_height();
    egui::Frame::NONE
        .fill(panel_fill)
        .stroke(panel_stroke)
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(content_margin, content_margin))
        .show(ui, |ui| {
            ui.set_min_height(list_height);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(6, 6))
                .show(ui, |ui| {
                    render_download_list(ui, ctx, app, frame, list_height);
                });
        });
}

fn render_search_section(
    // 検索画面の描画先UI
    ui: &mut egui::Ui,
    // 入力状態や再描画を扱うコンテキスト
    ctx: &egui::Context,
    // 検索入力の状態を保持するアプリ状態
    app: &mut DownloaderApp,
    // ネイティブドラッグなどフレーム操作に利用
    frame: &eframe::Frame,
) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Search")
            .size(13.0)
            .color(egui::Color32::from_rgb(226, 232, 240)),
    );
    ui.add_space(8.0);

    let changed = render_search_input(ui, app);
    if changed {
        app.mark_search_dirty();
    }
    ui.add_space(8.0);

    ui.label(
        egui::RichText::new("設定で検索対象フォルダ（外付けSSD等）を指定してください。")
            .size(11.5)
            .color(egui::Color32::from_rgb(130, 140, 160)),
    );
    ui.add_space(8.0);

    let list_height = ui.available_height();
    egui::Frame::NONE
        .fill(egui::Color32::from_rgb(24, 30, 45))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(36, 44, 62)))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(3, 3))
        .show(ui, |ui| {
            ui.set_min_height(list_height);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(10, 10))
                .show(ui, |ui| {
                    render_search_results_list(ui, ctx, app, frame, list_height);
                });
        });
}

fn render_search_input(
    // 検索入力欄の描画先UI
    ui: &mut egui::Ui,
    // 検索クエリ文字列を保持するアプリ状態
    app: &mut DownloaderApp,
) -> bool {
    let mut changed = false;
    egui::Frame::NONE
        .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 15))
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 36),
        ))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            let response = ui.add_sized(
                [ui.available_width(), 24.0],
                egui::TextEdit::singleline(&mut app.search_query)
                    .hint_text("ファイル名またはメタ情報で検索...")
                    .text_color(egui::Color32::from_rgb(226, 232, 240))
                    .frame(false),
            );
            if response.changed() {
                changed = true;
            }
        });
    changed
}

fn render_search_results_list(
    // 検索結果リストの描画先UI
    ui: &mut egui::Ui,
    // カーソル位置など入力情報の取得に使用
    ctx: &egui::Context,
    // 検索入力文字列の参照元
    app: &mut DownloaderApp,
    // ドラッグ開始時にOSへ通知するためのフレーム
    frame: &eframe::Frame,
    // 一覧の最大表示高さ
    list_height: f32,
) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(list_height)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if app.search_query.trim().is_empty() {
                return;
            }

            if let Some(err) = &app.search_error {
                ui.label(
                    egui::RichText::new(err)
                        .size(12.5)
                        .color(egui::Color32::from_rgb(248, 113, 113)),
                );
                return;
            }

            if app.search_results.is_empty() {
                ui.label(
                    egui::RichText::new("該当するファイルはありませんでした")
                        .size(12.5)
                        .color(egui::Color32::from_rgb(120, 130, 150)),
                );
                return;
            }

            let entries = app
                .search_results
                .iter()
                .map(|hit| (hit.file_name.clone(), hit.path.clone()))
                .collect::<Vec<_>>();
            let previous_spacing = ui.spacing().item_spacing;
            ui.spacing_mut().item_spacing = egui::vec2(previous_spacing.x, 0.0);
            let font_id = egui::FontId::proportional(13.5);
            let text_center_offset = measure_text_center_offset(ui, &font_id);

            for (file_name, path_string) in &entries {
                let row_width = (ui.available_width() - ui.spacing().scroll.bar_width).max(0.0);
                let row_height = 36.0;
                let row_padding_x = 12.0;
                let text_max_width = (row_width - row_padding_x * 2.0).max(0.0);
                let text = truncate_with_ellipsis(ui, file_name, text_max_width, &font_id);
                let path = std::path::PathBuf::from(path_string);

                let (row_rect, _) =
                    ui.allocate_exact_size(egui::vec2(row_width, row_height), egui::Sense::hover());
                let row_rect = row_rect.round_to_pixels(ctx.pixels_per_point());
                let base_fill = egui::Color32::from_rgb(24, 30, 45);
                let hover_fill = egui::Color32::from_rgb(24, 48, 70);
                let row_hovered = ctx.input(|i| {
                    i.pointer
                        .latest_pos()
                        .is_some_and(|pos| row_rect.contains(pos))
                });
                let fill = if row_hovered { hover_fill } else { base_fill };
                ui.painter()
                    .rect_filled(row_rect, egui::CornerRadius::same(0), fill);

                if row_hovered {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }

                let inner_rect = row_rect.shrink2(egui::vec2(row_padding_x, 0.0));
                let text_color = egui::Color32::from_rgb(220, 230, 245);
                let text_pos =
                    egui::pos2(inner_rect.left(), row_rect.center().y + text_center_offset);
                ui.painter().text(
                    text_pos,
                    egui::Align2::LEFT_CENTER,
                    text,
                    font_id.clone(),
                    text_color,
                );

                let drag_response = ui.interact(
                    row_rect,
                    ui.make_persistent_id((path_string, "search_drag_row")),
                    egui::Sense::drag(),
                );
                if drag_response.drag_started() {
                    app.start_native_drag(frame, &path);
                }
            }
            ui.spacing_mut().item_spacing = previous_spacing;
        });
}

fn render_download_list(
    // ダウンロード一覧の描画先UI
    ui: &mut egui::Ui,
    // カーソル位置など入力情報の取得に使用
    ctx: &egui::Context,
    // ダウンロード済みファイル一覧の参照元
    app: &mut DownloaderApp,
    // ドラッグ開始時にOSへ通知するためのフレーム
    frame: &eframe::Frame,
    // 一覧の最大表示高さ
    list_height: f32,
) {
    egui::ScrollArea::vertical()
        .max_height(list_height)
        .show(ui, |ui| {
            if app.downloaded_files.is_empty() {
                ui.label(
                    egui::RichText::new("まだダウンロードがありません。")
                        .size(12.5)
                        .color(egui::Color32::from_rgb(120, 130, 150)),
                );
                return;
            }
            let files = app.downloaded_files.clone();
            let mut remove_paths = Vec::new();
            let previous_spacing = ui.spacing().item_spacing;
            ui.spacing_mut().item_spacing = egui::vec2(previous_spacing.x, 0.0);
            let font_id = egui::FontId::proportional(13.5);
            let text_center_offset = measure_text_center_offset(ui, &font_id);
            for path in &files {
                let filename = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown");
                let row_width = (ui.available_width() - ui.spacing().scroll.bar_width).max(0.0);
                let row_height = 36.0;
                let row_padding_x = 12.0;
                let remove_width = 28.0;
                let remove_height = 28.0;
                let remove_spacing = 8.0;
                let text_max_width =
                    (row_width - row_padding_x * 2.0 - remove_width - remove_spacing).max(0.0);
                let text = truncate_with_ellipsis(ui, filename, text_max_width, &font_id);

                let (row_rect, _) =
                    ui.allocate_exact_size(egui::vec2(row_width, row_height), egui::Sense::hover());
                let row_rect = row_rect.round_to_pixels(ctx.pixels_per_point());
                let base_fill = egui::Color32::from_rgb(24, 30, 45);
                let hover_fill = egui::Color32::from_rgb(24, 48, 70);
                let row_hovered = ctx.input(|i| {
                    i.pointer
                        .latest_pos()
                        .map_or(false, |pos| row_rect.contains(pos))
                });
                let fill = if row_hovered { hover_fill } else { base_fill };
                ui.painter()
                    .rect_filled(row_rect, egui::CornerRadius::same(0), fill);

                if row_hovered {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }

                let inner_rect = row_rect.shrink2(egui::vec2(row_padding_x, 0.0));
                let text_color = egui::Color32::from_rgb(220, 230, 245);
                let text_pos =
                    egui::pos2(inner_rect.left(), row_rect.center().y + text_center_offset);
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
                let remove_hovered = ctx.input(|i| {
                    i.pointer
                        .latest_pos()
                        .is_some_and(|pos| remove_rect.contains(pos))
                });
                let remove_color = if remove_hovered {
                    egui::Color32::from_rgb(252, 165, 165)
                } else {
                    egui::Color32::from_rgb(200, 210, 230)
                };
                let remove_button = ui.put(
                    remove_rect,
                    egui::Button::new(egui::RichText::new("✕").size(15.0).color(remove_color))
                        .frame(false),
                );
                if remove_button.clicked() {
                    remove_paths.push(path.clone());
                }

                let drag_rect = {
                    let max_x = remove_button.rect.left().min(row_rect.right());
                    if max_x > row_rect.left() {
                        egui::Rect::from_min_max(row_rect.min, egui::pos2(max_x, row_rect.bottom()))
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
        });
}

fn render_progress_panel(
    // 進捗パネルの描画先UI
    ui: &mut egui::Ui,
    // アニメーション時間や再描画依頼に使用
    ctx: &egui::Context,
    // 進捗表示に必要な読み取り専用アプリ状態
    app: &DownloaderApp,
) {
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
    let label_color = apply_opacity(egui::Color32::from_rgb(203, 213, 225), opacity);

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
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(bar_width, bar_height), egui::Sense::hover());

            let track_color = apply_opacity(
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 31),
                opacity,
            );
            let bar_fill = apply_opacity(egui::Color32::from_rgb(56, 189, 248), opacity);
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
                        ui.painter().rect_filled(seg_rect, rounding, bar_fill);
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
                        ui.painter().rect_filled(fill_rect, rounding, bar_fill);
                    }
                }
            }
        });
}

fn apply_opacity(
    // ベースとなる色
    color: egui::Color32,
    // 0.0〜1.0の透過率
    opacity: f32,
) -> egui::Color32 {
    let alpha = (color.a() as f32 * opacity).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn truncate_with_ellipsis(
    // フォント計測に使うUI
    ui: &egui::Ui,
    // 表示したい元の文字列
    text: &str,
    // 収めたい最大幅（px）
    max_width: f32,
    // 計測に使うフォント指定
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

fn measure_text_center_offset(
    // フォント計測に使うUI
    ui: &mut egui::Ui,
    // 計測に使うフォント指定
    font_id: &egui::FontId,
) -> f32 {
    ui.fonts_mut(|fonts| {
        let galley = fonts.layout_no_wrap("Ag".to_string(), font_id.clone(), egui::Color32::WHITE);
        galley.rect.center().y - galley.mesh_bounds.center().y
    })
}

fn text_width(
    // フォント計測に使うUI
    ui: &egui::Ui,
    // 測定対象の文字列
    text: &str,
    // 計測に使うフォント指定
    font_id: &egui::FontId,
) -> f32 {
    ui.fonts_mut(|fonts| {
        let galley = fonts.layout_no_wrap(text.to_string(), font_id.clone(), egui::Color32::WHITE);
        galley.size().x
    })
}
