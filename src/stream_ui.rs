use eframe::egui;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Instant;

use crate::app::DownloaderApp;
use crate::cursor::pointing;
use crate::download::{ProcessTracker, read_clipboard_text};
use crate::stream::{
    PREVIEW_FPS, PREVIEW_HEIGHT, PREVIEW_WIDTH, StreamEvent, StreamFrame, resolve_and_run,
    run_from_urls,
};

// ジッタ吸収用フレームバッファの上限（暴走時の安全弁）。
const MAX_FRAME_BUFFER: usize = 120;

pub struct StreamUiState {
    pub show_stream: bool,
    running: bool,
    paused: bool,
    error: Option<String>,
    duration: Option<f64>,
    position: f64,
    position_instant: Option<Instant>,
    scrubbing: Option<f64>,
    media_urls: Vec<String>,
    run_id: u64,
    cancel_flag: Option<Arc<AtomicBool>>,
    tracker: Option<ProcessTracker>,
    texture: Option<egui::TextureHandle>,
    frame_buffer: VecDeque<StreamFrame>,
    tx: mpsc::Sender<StreamEvent>,
    rx: mpsc::Receiver<StreamEvent>,
    frame_tx: mpsc::Sender<StreamFrame>,
    frame_rx: mpsc::Receiver<StreamFrame>,
}

impl StreamUiState {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::channel();
        Self {
            show_stream: false,
            running: false,
            paused: false,
            error: None,
            duration: None,
            position: 0.0,
            position_instant: None,
            scrubbing: None,
            media_urls: Vec::new(),
            run_id: 0,
            cancel_flag: None,
            tracker: None,
            texture: None,
            frame_buffer: VecDeque::new(),
            tx,
            rx,
            frame_tx,
            frame_rx,
        }
    }

    pub fn open_stream(&mut self) {
        self.show_stream = true;
    }

    // クリップボードのURLを解決して再生を開始する。再生中なら新しい動画へ置き換える。
    fn start_stream(&mut self, cookie_args: Vec<String>) {
        let Some(url) = read_clipboard_text() else {
            self.error = Some("クリップボードにURLがありません。".to_string());
            return;
        };

        self.drain_frames();
        self.running = true;
        self.paused = false;
        self.error = None;
        self.duration = None;
        self.position = 0.0;
        self.position_instant = None;
        self.scrubbing = None;
        self.media_urls.clear();

        let (run_id, cancel, tracker) = self.begin_run();
        let tx = self.tx.clone();
        let frame_tx = self.frame_tx.clone();
        thread::spawn(move || {
            resolve_and_run(url, cookie_args, 0.0, run_id, tx, frame_tx, cancel, tracker);
        });
    }

    // 指定位置へシークし、ffmpeg を再起動する。
    fn seek(&mut self, target: f64) {
        if self.media_urls.is_empty() {
            return;
        }
        let target = match self.duration {
            Some(duration) => target.clamp(0.0, duration),
            None => target.max(0.0),
        };

        self.drain_frames();
        self.paused = false;
        self.position = target;
        self.position_instant = None;

        let urls = self.media_urls.clone();
        let (run_id, cancel, tracker) = self.begin_run();
        let tx = self.tx.clone();
        let frame_tx = self.frame_tx.clone();
        thread::spawn(move || {
            run_from_urls(urls, target, run_id, tx, frame_tx, cancel, tracker);
        });
    }

    // 現在位置から相対シークする（早送り/巻き戻し）。
    fn seek_relative(&mut self, delta: f64) {
        if !self.running || self.media_urls.is_empty() {
            return;
        }
        let target = (self.current_position() + delta).max(0.0);
        self.seek(target);
    }

    // 一時停止/再開を切り替える。
    fn toggle_pause(&mut self) {
        if !self.running {
            return;
        }
        if self.paused {
            if let Some(tracker) = self.tracker.as_ref() {
                tracker.resume_all();
            }
            self.paused = false;
            self.position_instant = Some(Instant::now());
        } else {
            // 現在位置を確定させてから停止する。
            self.position = self.current_position();
            self.position_instant = None;
            if let Some(tracker) = self.tracker.as_ref() {
                tracker.suspend_all();
            }
            self.paused = true;
        }
    }

    // 再生を停止して状態を初期化する。
    fn stop(&mut self) {
        self.run_id += 1;
        if let Some(tracker) = self.tracker.as_ref() {
            tracker.terminate_all();
        }
        self.cancel_flag = None;
        self.tracker = None;
        self.running = false;
        self.paused = false;
        self.texture = None;
        self.duration = None;
        self.position = 0.0;
        self.position_instant = None;
        self.scrubbing = None;
        self.media_urls.clear();
        self.drain_frames();
    }

    // 新しい再生世代を採番し、追跡用のフラグ/トラッカーを差し替える。
    fn begin_run(&mut self) -> (u64, Arc<AtomicBool>, ProcessTracker) {
        self.run_id += 1;
        if let Some(tracker) = self.tracker.as_ref() {
            tracker.terminate_all();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let tracker = ProcessTracker::new();
        self.cancel_flag = Some(cancel.clone());
        self.tracker = Some(tracker.clone());
        (self.run_id, cancel, tracker)
    }

    // 表示用の現在再生位置（秒）。再生中は前回位置からの経過で補間する。
    fn current_position(&self) -> f64 {
        if let Some(scrub) = self.scrubbing {
            return scrub;
        }
        let mut position = self.position;
        if self.running && !self.paused {
            if let Some(instant) = self.position_instant {
                position += instant.elapsed().as_secs_f64();
            }
        }
        if let Some(duration) = self.duration {
            position = position.clamp(0.0, duration);
        }
        position.max(0.0)
    }

    fn drain_frames(&mut self) {
        while self.frame_rx.try_recv().is_ok() {}
        self.frame_buffer.clear();
    }

    pub fn poll_updates(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                StreamEvent::Resolved {
                    run_id,
                    duration,
                    urls,
                } => {
                    if run_id == self.run_id {
                        self.duration = duration;
                        self.media_urls = urls;
                    }
                }
                StreamEvent::Position { run_id, secs } => {
                    if run_id == self.run_id && !self.paused {
                        self.position = secs;
                        self.position_instant = Some(Instant::now());
                    }
                }
                StreamEvent::Finished { run_id, result } => {
                    if run_id != self.run_id {
                        continue;
                    }
                    self.running = false;
                    self.paused = false;
                    self.cancel_flag = None;
                    self.tracker = None;
                    self.texture = None;
                    self.position_instant = None;
                    self.drain_frames();
                    if let Err(err) = result {
                        self.error = Some(err);
                    }
                }
            }
        }
    }
}

impl Default for StreamUiState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn render_stream_viewport(app: &mut DownloaderApp, ctx: &egui::Context) {
    if !app.stream_ui.show_stream {
        return;
    }

    let mut close_requested = false;
    let viewport_id = stream_viewport_id();
    let builder = egui::ViewportBuilder::default()
        .with_title("ストリーム再生")
        .with_inner_size(egui::vec2(560.0, 560.0))
        .with_min_inner_size(egui::vec2(480.0, 480.0))
        .with_always_on_top();

    ctx.show_viewport_immediate(viewport_id, builder, |ctx, class| {
        if ctx.input(|i| i.viewport().close_requested()) {
            close_requested = true;
        }

        match class {
            egui::ViewportClass::Embedded => {
                let mut open = true;
                egui::Window::new("ストリーム再生")
                    .collapsible(false)
                    .resizable(true)
                    .default_width(540.0)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        render_stream_contents(ui, app, ctx);
                    });
                if !open {
                    close_requested = true;
                }
            }
            _ => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_stream_contents(ui, app, ctx);
                });
            }
        }
    });

    if close_requested {
        app.stream_ui.stop();
        app.stream_ui.show_stream = false;
    }
}

fn render_stream_contents(ui: &mut egui::Ui, app: &mut DownloaderApp, ctx: &egui::Context) {
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 14,
            bottom: 16,
        })
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("ストリーム再生")
                    .size(18.0)
                    .strong()
                    .color(egui::Color32::from_rgb(220, 230, 245)),
            );
            ui.label(
                egui::RichText::new("クリップボードのURLをこのウィンドウ内で再生します。")
                    .size(12.0)
                    .color(egui::Color32::from_rgb(140, 150, 170)),
            );
            ui.add_space(12.0);

            update_preview_texture(app, ctx);
            render_preview(ui, app);
            ui.add_space(8.0);

            render_transport(ui, app);
            ui.add_space(8.0);

            render_seek_bar(ui, app);
            ui.add_space(10.0);

            render_controls(ui, app);

            if let Some(err) = &app.stream_ui.error {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(err)
                        .size(12.0)
                        .color(egui::Color32::from_rgb(248, 113, 113)),
                );
            }

            if app.stream_ui.running && !app.stream_ui.paused {
                ctx.request_repaint();
            }
        });
}

// 受信フレームをバッファへ取り込み、再生クロックに沿った1枚をテクスチャへ反映する。
fn update_preview_texture(app: &mut DownloaderApp, ctx: &egui::Context) {
    let run_id = app.stream_ui.run_id;

    // 現世代のフレームのみバッファへ蓄積する。
    while let Ok(frame) = app.stream_ui.frame_rx.try_recv() {
        if frame.run_id == run_id {
            app.stream_ui.frame_buffer.push_back(frame);
        }
    }
    while app.stream_ui.frame_buffer.len() > MAX_FRAME_BUFFER {
        app.stream_ui.frame_buffer.pop_front();
    }

    // 一時停止中・停止中は更新しない（直近フレームで静止）。
    if !app.stream_ui.running || app.stream_ui.paused {
        return;
    }

    let chosen = if app.stream_ui.position_instant.is_some() {
        // 再生クロック確立後は、現在時刻までに到達したフレームの最新を提示する。
        let clock = app.stream_ui.current_position();
        let tolerance = 0.5 / PREVIEW_FPS;
        let mut chosen = None;
        while app
            .stream_ui
            .frame_buffer
            .front()
            .is_some_and(|frame| frame.pts <= clock + tolerance)
        {
            chosen = app.stream_ui.frame_buffer.pop_front();
        }
        chosen
    } else {
        // クロック未確立（解析/バッファリング中）は最新フレームを即時提示する。
        let mut chosen = None;
        while let Some(frame) = app.stream_ui.frame_buffer.pop_front() {
            chosen = Some(frame);
        }
        chosen
    };

    let Some(frame) = chosen else {
        return;
    };
    let image = egui::ColorImage::from_rgba_unmultiplied(frame.size, &frame.rgba);
    match app.stream_ui.texture.as_mut() {
        Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
        None => {
            app.stream_ui.texture =
                Some(ctx.load_texture("stream_preview", image, egui::TextureOptions::LINEAR));
        }
    }
}

fn render_preview(ui: &mut egui::Ui, app: &DownloaderApp) {
    let width = ui.available_width();
    let height = width * PREVIEW_HEIGHT as f32 / PREVIEW_WIDTH as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let rounding = egui::CornerRadius::same(12);
    ui.painter().rect_filled(rect, rounding, egui::Color32::BLACK);

    if let Some(texture) = app.stream_ui.texture.as_ref() {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter()
            .image(texture.id(), rect, uv, egui::Color32::WHITE);
    } else {
        let message = if app.stream_ui.running {
            "読み込み中..."
        } else {
            "停止中"
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            message,
            egui::FontId::proportional(13.0),
            egui::Color32::from_rgb(120, 130, 150),
        );
    }
}

// ドラッグでシーク可能なシークバーと時間表示を描画する。
fn render_seek_bar(ui: &mut egui::Ui, app: &mut DownloaderApp) {
    let position = app.stream_ui.current_position();
    let duration = app.stream_ui.duration;
    let seekable = app.stream_ui.running && !app.stream_ui.media_urls.is_empty() && duration.is_some();

    let bar_height = 16.0;
    let width = ui.available_width();
    let sense = if seekable {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, bar_height), sense);

    let mut commit_target = None;
    if seekable {
        if let Some(dur) = duration {
            if (response.dragged() || response.clicked()) && dur > 0.0 {
                if let Some(pos) = response.interact_pointer_pos() {
                    let fraction = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                    app.stream_ui.scrubbing = Some(fraction as f64 * dur);
                }
            }
            if response.drag_stopped() || response.clicked() {
                commit_target = app.stream_ui.scrubbing.take();
            }
        }
    }

    let display_position = app.stream_ui.scrubbing.unwrap_or(position);
    let fraction = match duration {
        Some(dur) if dur > 0.0 => (display_position / dur).clamp(0.0, 1.0) as f32,
        _ => 0.0,
    };

    let track_center_y = rect.center().y;
    let track_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, track_center_y),
        egui::vec2(rect.width(), 6.0),
    );
    let rounding = egui::CornerRadius::same(3);
    ui.painter().rect_filled(
        track_rect,
        rounding,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 31),
    );

    if fraction > 0.0 {
        let filled = egui::Rect::from_min_max(
            track_rect.min,
            egui::pos2(track_rect.left() + track_rect.width() * fraction, track_rect.bottom()),
        );
        ui.painter()
            .rect_filled(filled, rounding, egui::Color32::from_rgb(56, 189, 248));
    }

    if seekable {
        let handle_x = track_rect.left() + track_rect.width() * fraction;
        ui.painter().circle_filled(
            egui::pos2(handle_x, track_center_y),
            6.0,
            egui::Color32::from_rgb(125, 211, 252),
        );
        if response.hovered() || response.dragged() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        }
    }

    if let Some(target) = commit_target {
        app.stream_ui.seek(target);
    }

    ui.add_space(4.0);
    let total_label = match duration {
        Some(dur) => format_time(dur),
        None => "--:--".to_string(),
    };
    ui.label(
        egui::RichText::new(format!("{} / {}", format_time(display_position), total_label))
            .size(11.5)
            .color(egui::Color32::from_rgb(170, 180, 200)),
    );
}

// 再生/巻き戻し/早送りのアイコンボタン行をプレビュー直下に表示する。
fn render_transport(ui: &mut egui::Ui, app: &mut DownloaderApp) {
    let button_size = egui::vec2(56.0, 40.0);
    let gap = 12.0;
    let content_width = button_size.x * 3.0 + gap * 2.0;
    let leading = ((ui.available_width() - content_width) * 0.5).max(0.0);

    ui.horizontal(|ui| {
        ui.add_space(leading);
        if icon_button(ui, TransportIcon::Rewind, button_size).clicked() {
            app.stream_ui.seek_relative(-0.5);
        }
        ui.add_space(gap);
        let play_icon = if app.stream_ui.running && !app.stream_ui.paused {
            TransportIcon::Pause
        } else {
            TransportIcon::Play
        };
        if icon_button(ui, play_icon, button_size).clicked() {
            app.stream_ui.toggle_pause();
        }
        ui.add_space(gap);
        if icon_button(ui, TransportIcon::Forward, button_size).clicked() {
            app.stream_ui.seek_relative(0.5);
        }
    });
}

enum TransportIcon {
    Rewind,
    Play,
    Pause,
    Forward,
}

// アイコンを描画するボタン。背景＋ベクター図形でアイコンを描く。
fn icon_button(ui: &mut egui::Ui, icon: TransportIcon, size: egui::Vec2) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let background = if response.hovered() {
        egui::Color32::from_rgb(40, 56, 78)
    } else {
        egui::Color32::from_rgb(30, 38, 56)
    };
    let color = egui::Color32::from_rgb(224, 232, 246);
    let center = rect.center();
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(10), background);

    match icon {
        TransportIcon::Play => draw_triangle(painter, center, 9.0, true, color),
        TransportIcon::Pause => {
            let bar = egui::vec2(4.5, 16.0);
            let offset = 4.5;
            let rounding = egui::CornerRadius::same(2);
            painter.rect_filled(
                egui::Rect::from_center_size(egui::pos2(center.x - offset, center.y), bar),
                rounding,
                color,
            );
            painter.rect_filled(
                egui::Rect::from_center_size(egui::pos2(center.x + offset, center.y), bar),
                rounding,
                color,
            );
        }
        TransportIcon::Rewind => {
            draw_triangle(painter, egui::pos2(center.x - 5.0, center.y), 7.0, false, color);
            draw_triangle(painter, egui::pos2(center.x + 6.0, center.y), 7.0, false, color);
        }
        TransportIcon::Forward => {
            draw_triangle(painter, egui::pos2(center.x - 6.0, center.y), 7.0, true, color);
            draw_triangle(painter, egui::pos2(center.x + 5.0, center.y), 7.0, true, color);
        }
    }

    pointing(response)
}

// 再生/シーク用の三角アイコンを描く。
fn draw_triangle(
    painter: &egui::Painter,
    center: egui::Pos2,
    half: f32,
    pointing_right: bool,
    color: egui::Color32,
) {
    let points = if pointing_right {
        vec![
            egui::pos2(center.x - half * 0.8, center.y - half),
            egui::pos2(center.x - half * 0.8, center.y + half),
            egui::pos2(center.x + half, center.y),
        ]
    } else {
        vec![
            egui::pos2(center.x + half * 0.8, center.y - half),
            egui::pos2(center.x + half * 0.8, center.y + half),
            egui::pos2(center.x - half, center.y),
        ]
    };
    painter.add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
}

// プレビュー下部のメインボタン（停止中は開始、再生中はクリップボードURLで置換）。
fn render_controls(ui: &mut egui::Ui, app: &mut DownloaderApp) {
    let label = if app.stream_ui.running {
        "ストリーム"
    } else {
        "ストリーム開始"
    };
    let button = egui::Button::new(
        egui::RichText::new(label)
            .size(13.0)
            .color(egui::Color32::from_rgb(8, 14, 24)),
    )
    .fill(egui::Color32::from_rgb(56, 189, 248))
    .corner_radius(egui::CornerRadius::same(12));
    if pointing(ui.add_sized([ui.available_width(), 44.0], button)).clicked() {
        let cookie_args = app.settings_ui.cookie_args();
        app.stream_ui.start_stream(cookie_args);
    }
}

fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}

fn stream_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("stream_viewport")
}
