use eframe::egui;

pub fn apply_theme(
    // テーマ適用先のeguiコンテキスト
    ctx: &egui::Context,
) {
    let mut style = (*ctx.style()).clone();
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
    style.visuals.selection.bg_fill = egui::Color32::from_rgb(16, 190, 255);
    style.visuals.hyperlink_color = egui::Color32::from_rgb(16, 190, 255);
    style.spacing.item_spacing = egui::vec2(12.0, 10.0);
    style.spacing.button_padding = egui::vec2(14.0, 10.0);
    ctx.set_style(style);

    let mut fonts = egui::FontDefinitions::default();
    install_fonts(&mut fonts);
    ctx.set_fonts(fonts);
}

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
}

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
