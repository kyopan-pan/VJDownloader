use eframe::egui;

/// Deferred Viewport のルート領域をテーマのパネル色で塗る。
///
/// `show_viewport_deferred` が渡す `Ui` は `CentralPanel` を自動生成しないため、
/// 明示的に塗らないとネイティブウィンドウのクリアカラーが露出する。
pub fn paint_viewport_background(ui: &egui::Ui) {
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
}

pub fn apply_theme(
    // テーマ適用先のeguiコンテキスト
    ctx: &egui::Context,
) {
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.window_fill = egui::Color32::from_rgb(12, 18, 32);
    style.visuals.panel_fill = egui::Color32::from_rgb(12, 18, 32);
    style.visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(20, 28, 44);
    style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(20, 28, 44);
    style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(26, 34, 54);
    style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(32, 42, 66);
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(10);
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(10);
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(10);
    style.visuals.widgets.inactive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 70, 90));
    // 日本語IMEの変換中ハイライトが強く出ないようにしつつ、選択文字は白で可読性を保つ。
    style.visuals.selection.bg_fill = egui::Color32::from_rgb(52, 62, 84);
    style.visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    style.visuals.hyperlink_color = egui::Color32::from_rgb(16, 190, 255);
    style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    style.spacing.item_spacing = egui::vec2(12.0, 10.0);
    style.spacing.button_padding = egui::vec2(14.0, 10.0);
    style.spacing.scroll = egui::style::ScrollStyle::floating();
    style.spacing.scroll.bar_outer_margin = 0.0;
    ctx.set_style_of(egui::Theme::Dark, style);

    let mut fonts = egui::FontDefinitions::default();
    install_fonts(&mut fonts);
    ctx.set_fonts(fonts);
}

#[cfg(not(target_os = "windows"))]
fn install_fonts(
    // 登録済みフォント定義への追加先
    fonts: &mut egui::FontDefinitions,
) {
    let brand_candidates = [
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/SFNSDisplay.ttf",
        "/System/Library/Fonts/SFNSText.ttf",
        "/Library/Fonts/Avenir Next.ttf",
        "/Library/Fonts/AvenirNext-Regular.ttf",
    ];

    let japanese_candidates = [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
        "/System/Library/Fonts/CJKSymbolsFallback.ttc",
    ];

    if let Some(font_data) = load_first_font(&brand_candidates) {
        fonts
            .font_data
            .insert("brand".to_string(), font_data.into());
    }

    if let Some(font_data) = load_first_font(&japanese_candidates) {
        fonts.font_data.insert("jp".to_string(), font_data.into());
    }

    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        let mut insert_at = 0;
        if fonts.font_data.contains_key("brand") {
            family.insert(insert_at, "brand".to_string());
            insert_at += 1;
        }
        if fonts.font_data.contains_key("jp") {
            family.insert(insert_at, "jp".to_string());
        }
    }

    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace)
        && fonts.font_data.contains_key("jp")
    {
        family.push("jp".to_string());
    }
}

#[cfg(not(target_os = "windows"))]
fn load_first_font(
    // 探索するフォントファイル候補一覧
    paths: &[&str],
) -> Option<egui::FontData> {
    for path in paths {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(egui::FontData::from_owned(bytes));
        }
    }
    None
}

// Windowsでは英字と日本語に同じフォントを優先してベースラインを揃える。
#[cfg(target_os = "windows")]
fn install_fonts(fonts: &mut egui::FontDefinitions) {
    let windows_dir = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("WINDIR"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    let fonts_dir = windows_dir.join("Fonts");
    for name in ["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc"] {
        if let Ok(bytes) = std::fs::read(fonts_dir.join(name)) {
            fonts.font_data.insert(
                "windows-jp".to_string(),
                egui::FontData::from_owned(bytes).into(),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, "windows-jp".to_string());
            }
            return;
        }
    }
    eprintln!(
        "日本語フォントを読み込めませんでした: {}",
        fonts_dir.display()
    );
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn windows_fonts_render_japanese_in_both_families() {
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.fonts_mut(|fonts| {
                for font in [
                    egui::FontId::proportional(14.0),
                    egui::FontId::monospace(14.0),
                ] {
                    let galley = fonts.layout_no_wrap(
                        "ABCyt-dlp待機中ダウンロード検索設定日本語".to_string(),
                        font,
                        egui::Color32::WHITE,
                    );
                    let glyphs = &galley.rows[0].glyphs;
                    let first = &glyphs[0];
                    for glyph in glyphs {
                        assert_eq!(glyph.font_face_ascent, first.font_face_ascent);
                        assert_eq!(glyph.font_face_height, first.font_face_height);
                        assert!(glyph.advance_width > 0.0);
                    }
                    // 異なる漢字が同じ代替グリフに置換されていないことも確認する。
                    let end = glyphs.len();
                    assert_ne!(glyphs[end - 1].uv_rect, glyphs[end - 2].uv_rect);
                }
            });
        });
    }
}
