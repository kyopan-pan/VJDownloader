// インデックス作成の進捗を出すミニウィンドウ。
//
// フェーズ2（動画コメントの後埋め）は10万件規模だと数時間続く。現場では本体を出しっぱなしに
// するため、設定画面を閉じても進み具合を追えるよう独立したウィンドウとして出す。
// 表示中も検索・ダウンロードは通常どおり使える。

use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::theme::paint_viewport_background;

use super::IndexProgress;

pub struct IndexProgressUiState {
    progress: IndexProgress,
    visible: bool,
    // 利用者が閉じたウィンドウを毎フレーム開き直さないためのラッチ。
    // 次のインデックス作成が始まったら解除する。
    dismissed: bool,
}

impl IndexProgressUiState {
    pub fn new() -> Self {
        Self {
            progress: IndexProgress::default(),
            visible: false,
            dismissed: false,
        }
    }

    // 毎フレーム、エンジンから読み取った進捗を反映する。
    pub fn update(&mut self, progress: IndexProgress) {
        let was_active = self.progress.is_active();
        self.progress = progress;

        if !self.progress.is_active() {
            // 完了したら閉じ、次回のために取り消しラッチも戻す。
            self.visible = false;
            self.dismissed = false;
            return;
        }

        if !was_active {
            self.dismissed = false;
        }
        self.visible = !self.dismissed;
    }
}

impl Default for IndexProgressUiState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn render_index_progress_viewport(
    state: &Arc<Mutex<IndexProgressUiState>>,
    ctx: &egui::Context,
) {
    if !state.lock().is_ok_and(|state| state.visible) {
        return;
    }

    let builder = egui::ViewportBuilder::default()
        .with_title("インデックス作成中")
        .with_inner_size(egui::vec2(400.0, 190.0))
        .with_min_inner_size(egui::vec2(360.0, 170.0))
        .with_resizable(false)
        .with_always_on_top();

    let state = Arc::clone(state);
    ctx.show_viewport_deferred(index_progress_viewport_id(), builder, move |ui, _class| {
        paint_viewport_background(ui);
        let ctx = ui.ctx().clone();
        let Ok(mut state) = state.lock() else { return };
        if ctx.input(|i| i.viewport().close_requested()) {
            state.visible = false;
            state.dismissed = true;
            return;
        }
        render_contents(ui, state.progress);
        // 進捗は本体ウィンドウ側のフレームで更新されるため、こちらも定期的に描き直す。
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    });
}

fn render_contents(ui: &mut egui::Ui, progress: IndexProgress) {
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 14,
            bottom: 16,
        })
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("検索インデックスを作成しています")
                    .size(14.0)
                    .strong()
                    .color(egui::Color32::from_rgb(220, 230, 245)),
            );
            ui.add_space(10.0);

            if progress.scanning_roots > 0 {
                render_phase_label(ui, "1. ファイル名を収集中");
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(13.0));
                    ui.label(
                        egui::RichText::new(format!("{} 件", format_count(progress.scanned_files)))
                            .size(12.5)
                            .color(egui::Color32::from_rgb(200, 212, 230)),
                    );
                });
                ui.add_space(10.0);
            }

            if progress.comment_running {
                render_phase_label(ui, "2. 動画コメントを読み込み中");
                let total = progress.comment_done + progress.comment_pending;
                let bar = egui::ProgressBar::new(progress.comment_ratio().unwrap_or(0.0))
                    .desired_height(8.0)
                    .corner_radius(egui::CornerRadius::same(4))
                    .fill(egui::Color32::from_rgb(56, 189, 248));
                ui.add(bar);
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} / {} 件",
                        format_count(progress.comment_done),
                        format_count(total)
                    ))
                    .size(12.5)
                    .color(egui::Color32::from_rgb(200, 212, 230)),
                );
                ui.add_space(10.0);
            }

            ui.label(
                egui::RichText::new(
                    "作成中もダウンロードと検索はそのまま使えます。\n\
                     コメント検索は読み込みが済んだ動画から順に使えるようになります。",
                )
                .size(11.0)
                .color(egui::Color32::from_rgb(140, 150, 170)),
            );
        });
}

fn render_phase_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(12.0)
            .color(egui::Color32::from_rgb(150, 175, 205)),
    );
    ui.add_space(3.0);
}

// 件数は3桁区切りで出す。数万〜十数万件になるため区切りがないと読み取れない。
fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn index_progress_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("index_progress_viewport")
}

#[cfg(test)]
mod tests {
    use super::{IndexProgress, IndexProgressUiState, format_count};

    fn scanning() -> IndexProgress {
        IndexProgress {
            scanning_roots: 1,
            ..Default::default()
        }
    }

    #[test]
    fn formats_counts_with_thousand_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(124_779), "124,779");
    }

    #[test]
    fn opens_while_indexing_and_closes_on_completion() {
        let mut state = IndexProgressUiState::new();
        assert!(!state.visible);

        state.update(scanning());
        assert!(state.visible);

        // フェーズ1が終わってフェーズ2へ移っても開いたままにする。
        state.update(IndexProgress {
            comment_running: true,
            comment_done: 10,
            comment_pending: 90,
            ..Default::default()
        });
        assert!(state.visible);

        state.update(IndexProgress::default());
        assert!(!state.visible);
    }

    #[test]
    fn stays_closed_after_dismiss_until_next_run() {
        let mut state = IndexProgressUiState::new();
        state.update(scanning());
        state.dismissed = true;
        state.visible = false;

        // 同じ作成が続いている間は開き直さない。
        state.update(scanning());
        assert!(!state.visible);

        // いったん完了し、次の作成が始まったら再び表示する。
        state.update(IndexProgress::default());
        state.update(scanning());
        assert!(state.visible);
    }
}
