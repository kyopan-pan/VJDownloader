use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Instant;

use crate::app::DownloaderApp;
use crate::cursor::pointing;
use crate::download::{ProcessTracker, read_clipboard_text};
use crate::stream::{PREVIEW_HEIGHT, PREVIEW_WIDTH, StreamEvent, StreamFrame, run_stream};

pub struct StreamUiState {
    pub show_stream: bool,
    running: bool,
    status: String,
    error: Option<String>,
    started_at: Option<Instant>,
    cancel_flag: Option<Arc<AtomicBool>>,
    tracker: Option<ProcessTracker>,
    texture: Option<egui::TextureHandle>,
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
            status: "待機中...".to_string(),
            error: None,
            started_at: None,
            cancel_flag: None,
            tracker: None,
            texture: None,
            tx,
            rx,
            frame_tx,
            frame_rx,
        }
    }

    pub fn open_stream(&mut self) {
        self.show_stream = true;
    }

    // クリップボードのURLを取得し、ウィンドウ内プレビューで再生を開始する。
    fn start_stream(&mut self, cookie_args: Vec<String>) {
        if self.running {
            return;
        }
        let Some(url) = read_clipboard_text() else {
            self.error = Some("クリップボードにURLがありません。".to_string());
            return;
        };

        self.drain_frames();
        self.running = true;
        self.error = None;
        self.status = "再生準備中...".to_string();
        self.started_at = Some(Instant::now());

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let tracker = ProcessTracker::new();
        self.cancel_flag = Some(cancel_flag.clone());
        self.tracker = Some(tracker.clone());

        let tx = self.tx.clone();
        let frame_tx = self.frame_tx.clone();
        thread::spawn(move || {
            run_stream(url, cookie_args, tx, frame_tx, cancel_flag, tracker);
        });
    }

    // 再生中のプロセスを終了させる。
    fn stop_stream(&mut self) {
        if let Some(flag) = self.cancel_flag.as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(tracker) = self.tracker.as_ref() {
            tracker.terminate_all();
        }
        self.status = "停止中...".to_string();
    }

    // 未処理のプレビューフレームを捨てる。
    fn drain_frames(&self) {
        while self.frame_rx.try_recv().is_ok() {}
    }

    pub fn poll_updates(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                StreamEvent::Log(line) => {
                    self.status = line;
                }
                StreamEvent::Finished(result) => {
                    self.running = false;
                    self.started_at = None;
                    self.cancel_flag = None;
                    self.tracker = None;
                    self.texture = None;
                    self.drain_frames();
                    match result {
                        Ok(()) => {
                            self.status = "再生を終了しました。".to_string();
                        }
                        Err(err) => {
                            self.status = "待機中...".to_string();
                            self.error = Some(err);
                        }
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
        .with_inner_size(egui::vec2(560.0, 520.0))
        .with_min_inner_size(egui::vec2(480.0, 440.0))
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
            ui.add_space(10.0);

            render_status_panel(ui, app);
            ui.add_space(12.0);

            let (label, fill) = if app.stream_ui.running {
                ("停止", egui::Color32::from_rgb(248, 113, 113))
            } else {
                ("ストリーム開始", egui::Color32::from_rgb(56, 189, 248))
            };
            let button = egui::Button::new(
                egui::RichText::new(label)
                    .size(13.0)
                    .color(egui::Color32::from_rgb(8, 14, 24)),
            )
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(12));

            if pointing(ui.add_sized([ui.available_width(), 44.0], button)).clicked() {
                if app.stream_ui.running {
                    app.stream_ui.stop_stream();
                } else {
                    let cookie_args = app.settings_ui.cookie_args();
                    app.stream_ui.start_stream(cookie_args);
                }
            }

            if app.stream_ui.running {
                ctx.request_repaint();
            }
        });
}

// 受信済みフレームのうち最新のものをテクスチャへ反映する。
fn update_preview_texture(app: &mut DownloaderApp, ctx: &egui::Context) {
    let mut latest = None;
    while let Ok(frame) = app.stream_ui.frame_rx.try_recv() {
        latest = Some(frame);
    }
    let Some(frame) = latest else {
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
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
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

fn render_status_panel(ui: &mut egui::Ui, app: &DownloaderApp) {
    egui::Frame::NONE
        .fill(egui::Color32::from_rgb(20, 26, 40))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 56, 78)))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            if let Some(err) = &app.stream_ui.error {
                ui.label(
                    egui::RichText::new(err)
                        .size(12.5)
                        .color(egui::Color32::from_rgb(248, 113, 113)),
                );
                return;
            }

            if app.stream_ui.running {
                let elapsed = app
                    .stream_ui
                    .started_at
                    .map(|started| started.elapsed().as_secs_f32())
                    .unwrap_or(0.0);
                ui.label(
                    egui::RichText::new(format!("再生中... {elapsed:.0}s"))
                        .size(13.0)
                        .strong()
                        .color(egui::Color32::from_rgb(125, 211, 252)),
                );
                ui.add_space(4.0);
            }

            ui.label(
                egui::RichText::new(&app.stream_ui.status)
                    .size(12.0)
                    .color(egui::Color32::from_rgb(203, 213, 225)),
            );
        });
}

fn stream_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("stream_viewport")
}
