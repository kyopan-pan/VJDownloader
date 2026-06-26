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
// 高解像度(Syphon)時は1フレームが大きいためメモリを抑える。
#[cfg(feature = "syphon")]
const MAX_FRAME_BUFFER: usize = 12;
#[cfg(not(feature = "syphon"))]
const MAX_FRAME_BUFFER: usize = 120;

// 1 つの再生デッキ。A/B それぞれが独立した ffmpeg パイプラインと状態を持つ。
struct StreamDeck {
    label: &'static str,
    texture_name: &'static str,
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
    // マスター合成（Syphon配信）用に直近の提示フレームを保持する。
    #[cfg(feature = "syphon")]
    last_rgba: Option<Vec<u8>>,
}

impl StreamDeck {
    fn new(label: &'static str, texture_name: &'static str) -> Self {
        let (tx, rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::channel();
        Self {
            label,
            texture_name,
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
            #[cfg(feature = "syphon")]
            last_rgba: None,
        }
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
    //
    // SIGSTOP で ffmpeg を凍結する方式だと、CoreAudio に積まれた先読み音声バッファが
    // 解放されず再生され続け（音声がループする）ため、一時停止では ffmpeg を正常終了させ、
    // 再開時に現在位置から再起動する（シークと同じ仕組み）。正常終了時に ffmpeg が
    // オーディオキューを破棄するため、停止直後の音声ループが起きない。
    fn toggle_pause(&mut self) {
        if !self.running {
            return;
        }
        if self.paused {
            self.resume_from_pause();
        } else {
            // 現在位置を確定させ、ffmpeg を停止して静止する。
            self.position = self.current_position();
            self.position_instant = None;
            // 旧プロセスの Finished イベントを無視するため世代を進めてから終了させる。
            self.run_id += 1;
            if let Some(tracker) = self.tracker.as_ref() {
                tracker.terminate_all();
            }
            self.cancel_flag = None;
            self.tracker = None;
            self.paused = true;
            self.drain_frames();
        }
    }

    // 一時停止中の位置から ffmpeg を再起動して再生を再開する。
    fn resume_from_pause(&mut self) {
        if self.media_urls.is_empty() {
            // URL 未解決などで再開できない場合は単に再生状態へ戻す。
            self.paused = false;
            return;
        }
        let urls = self.media_urls.clone();
        let target = self.position;
        self.paused = false;
        self.position_instant = None;

        let (run_id, cancel, tracker) = self.begin_run();
        let tx = self.tx.clone();
        let frame_tx = self.frame_tx.clone();
        thread::spawn(move || {
            run_from_urls(urls, target, run_id, tx, frame_tx, cancel, tracker);
        });
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
        #[cfg(feature = "syphon")]
        {
            self.last_rgba = None;
        }
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

    // 再生中（一時停止していない）かどうか。再描画要求の判定に使う。
    fn is_active(&self) -> bool {
        self.running && !self.paused
    }

    fn poll_updates(&mut self) {
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

    // 受信フレームをバッファへ取り込み、再生クロックに沿った1枚をテクスチャへ反映する。
    fn update_texture(&mut self, ctx: &egui::Context) {
        let run_id = self.run_id;

        // 現世代のフレームのみバッファへ蓄積する。
        while let Ok(frame) = self.frame_rx.try_recv() {
            if frame.run_id == run_id {
                self.frame_buffer.push_back(frame);
            }
        }
        while self.frame_buffer.len() > MAX_FRAME_BUFFER {
            self.frame_buffer.pop_front();
        }

        // 一時停止中・停止中は更新しない（直近フレームで静止）。
        if !self.running || self.paused {
            return;
        }

        let chosen = if self.position_instant.is_some() {
            // 再生クロック確立後は、現在時刻までに到達したフレームの最新を提示する。
            let clock = self.current_position();
            let tolerance = 0.5 / PREVIEW_FPS;
            let mut chosen = None;
            while self
                .frame_buffer
                .front()
                .is_some_and(|frame| frame.pts <= clock + tolerance)
            {
                chosen = self.frame_buffer.pop_front();
            }
            chosen
        } else {
            // クロック未確立（解析/バッファリング中）は最新フレームを即時提示する。
            let mut chosen = None;
            while let Some(frame) = self.frame_buffer.pop_front() {
                chosen = Some(frame);
            }
            chosen
        };

        let Some(frame) = chosen else {
            return;
        };
        let image = egui::ColorImage::from_rgba_unmultiplied(frame.size, &frame.rgba);
        match self.texture.as_mut() {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture(self.texture_name, image, egui::TextureOptions::LINEAR));
            }
        }
        // マスター合成用に直近フレームを保持（ColorImage は上でコピー済みなので move 可）。
        #[cfg(feature = "syphon")]
        {
            self.last_rgba = Some(frame.rgba);
        }
    }
}

pub struct StreamUiState {
    pub show_stream: bool,
    deck_a: StreamDeck,
    deck_b: StreamDeck,
    // マスタークロスフェード。0.0 = A のみ、1.0 = B のみ。
    fader: f32,
    // Syphon 出力（マスターを VDMX 等へ送信）の有効/無効とサーバ実体。
    #[cfg(feature = "syphon")]
    syphon_enabled: bool,
    #[cfg(feature = "syphon")]
    syphon: Option<crate::stream::syphon::SyphonPublisher>,
}

impl StreamUiState {
    pub fn new() -> Self {
        Self {
            show_stream: false,
            deck_a: StreamDeck::new("A", "stream_preview_a"),
            deck_b: StreamDeck::new("B", "stream_preview_b"),
            fader: 0.0,
            #[cfg(feature = "syphon")]
            syphon_enabled: false,
            #[cfg(feature = "syphon")]
            syphon: None,
        }
    }

    // マスター（A*(1-f)+B*f）を BGRA8 でCPU合成する。Syphon配信用。
    #[cfg(feature = "syphon")]
    fn master_bgra(&self) -> Vec<u8> {
        let w = PREVIEW_WIDTH;
        let h = PREVIEW_HEIGHT;
        let count = w * h;
        let f = self.fader.clamp(0.0, 1.0);
        let inv = 1.0 - f;
        let a = self.deck_a.last_rgba.as_deref();
        let b = self.deck_b.last_rgba.as_deref();
        let mut out = vec![0u8; count * 4];
        for i in 0..count {
            let p = i * 4;
            let (ar, ag, ab) = rgb_at(a, p);
            let (br, bg, bb) = rgb_at(b, p);
            let r = (ar as f32 * inv + br as f32 * f) as u8;
            let g = (ag as f32 * inv + bg as f32 * f) as u8;
            let bl = (ab as f32 * inv + bb as f32 * f) as u8;
            // Syphon(Metal) は BGRA8 を期待するため並びを入れ替える。
            out[p] = bl;
            out[p + 1] = g;
            out[p + 2] = r;
            out[p + 3] = 255;
        }
        out
    }

    pub fn open_stream(&mut self) {
        self.show_stream = true;
    }

    fn stop_all(&mut self) {
        self.deck_a.stop();
        self.deck_b.stop();
    }

    pub fn poll_updates(&mut self) {
        self.deck_a.poll_updates();
        self.deck_b.poll_updates();
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
        .with_inner_size(egui::vec2(940.0, 640.0))
        .with_min_inner_size(egui::vec2(760.0, 540.0))
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
                    .default_width(900.0)
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
        app.stream_ui.stop_all();
        app.stream_ui.show_stream = false;
    }
}

fn render_stream_contents(ui: &mut egui::Ui, app: &mut DownloaderApp, ctx: &egui::Context) {
    // 読み込みに使う cookie 引数を先に取り出して、以後は stream_ui を排他借用する。
    let cookie_args = app.settings_ui.cookie_args();
    let stream = &mut app.stream_ui;

    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 14,
            bottom: 16,
        })
        .show(ui, |ui| {
            ui.add_space(8.0);

            stream.deck_a.update_texture(ctx);
            stream.deck_b.update_texture(ctx);

            // プレビュー行とコントロール行で同じ3分割（A幅｜マスター幅｜B幅）を共有し、
            // 各デッキの操作UIがそのプレビューの真下に揃うようにする。
            // 丸め誤差での横オーバーフローを避けるため、合計幅にわずかな余白を残す。
            let gap = 12.0;
            let avail = ui.available_width();
            let deck_w = ((avail - gap * 2.0 - 4.0) / 2.55).floor().max(120.0);
            let master_w = deck_w * 0.55;

            let active = stream.deck_a.is_active() || stream.deck_b.is_active();

            // フェーダーは中央カラムに置くため、デッキは不変・フェーダーは可変で同時借用する
            //（いずれも別フィールドなので分離借用が成立する）。
            render_top_row(
                ui,
                &stream.deck_a,
                &stream.deck_b,
                &mut stream.fader,
                deck_w,
                master_w,
                gap,
            );
            ui.add_space(8.0);
            render_controls_row(
                ui,
                &mut stream.deck_a,
                &mut stream.deck_b,
                &cookie_args,
                deck_w,
                master_w,
                gap,
            );

            // Syphon 出力トグルとマスター配信（フィーチャー有効時のみ）。
            #[cfg(feature = "syphon")]
            {
                render_syphon_toggle(ui, stream);
                publish_master(stream, ctx);
            }

            if active {
                ctx.request_repaint();
            }
        });
}

// last_rgba から RGB を取り出す（範囲外/未保持は黒）。
#[cfg(feature = "syphon")]
fn rgb_at(buf: Option<&[u8]>, p: usize) -> (u8, u8, u8) {
    match buf {
        Some(b) if p + 2 < b.len() => (b[p], b[p + 1], b[p + 2]),
        _ => (0, 0, 0),
    }
}

// Syphon 出力の ON/OFF トグル。
#[cfg(feature = "syphon")]
fn render_syphon_toggle(ui: &mut egui::Ui, stream: &mut StreamUiState) {
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        ui.checkbox(
            &mut stream.syphon_enabled,
            "Syphon出力（マスターをVDMX等へ送信）",
        );
        if stream.syphon_enabled {
            let status = if stream.syphon.is_some() {
                "配信中: VJDownloader Master"
            } else {
                "初期化待ち…（Syphon.framework 未リンク時は無効）"
            };
            ui.label(
                egui::RichText::new(status)
                    .size(11.0)
                    .color(egui::Color32::from_rgb(150, 160, 180)),
            );
        }
    });
    // OFF にしたらサーバを破棄。
    if !stream.syphon_enabled {
        stream.syphon = None;
    }
}

// マスターを Syphon サーバへ1フレーム配信する。
#[cfg(feature = "syphon")]
fn publish_master(stream: &mut StreamUiState, ctx: &egui::Context) {
    if !stream.syphon_enabled {
        return;
    }
    if stream.syphon.is_none() {
        stream.syphon = crate::stream::syphon::SyphonPublisher::new(
            PREVIEW_WIDTH,
            PREVIEW_HEIGHT,
            "VJDownloader Master",
        );
    }
    if let Some(publisher) = stream.syphon.as_ref() {
        let bgra = stream.master_bgra();
        publisher.publish(&bgra);
        // 連続配信のため再描画を要求する。
        ctx.request_repaint();
    }
}

// 上段: A プレビュー / 中央カラム（小マスタープレビュー＋直下にフェーダー） / B プレビュー。
// 中央カラムは大プレビューの高さに対して縦中央へ寄せ、フェーダーは小プレビューの真下に密着させる。
fn render_top_row(
    ui: &mut egui::Ui,
    deck_a: &StreamDeck,
    deck_b: &StreamDeck,
    fader: &mut f32,
    deck_w: f32,
    master_w: f32,
    gap: f32,
) {
    let deck_h = deck_w * PREVIEW_HEIGHT as f32 / PREVIEW_WIDTH as f32;
    let master_h = master_w * PREVIEW_HEIGHT as f32 / PREVIEW_WIDTH as f32;
    let fader_h = 22.0;
    let group_h = master_h + 6.0 + fader_h;
    let top_pad = ((deck_h - group_h) * 0.5).max(0.0);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        render_deck_preview(ui, deck_a, egui::vec2(deck_w, deck_h));
        ui.add_space(gap);
        // 中央カラム: 小マスタープレビュー＋直下フェーダーを縦に並べ、縦中央へ配置。
        ui.allocate_ui_with_layout(
            egui::vec2(master_w, deck_h),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(top_pad);
                render_master_preview(ui, deck_a, deck_b, *fader, egui::vec2(master_w, master_h));
                ui.add_space(6.0);
                render_master_fader(ui, fader);
            },
        );
        ui.add_space(gap);
        render_deck_preview(ui, deck_b, egui::vec2(deck_w, deck_h));
    });
}

// コントロール行。プレビューと同じ3分割で A操作UI ｜ （中央は空き） ｜ B操作UI を並べ、
// 各セル幅がプレビュー幅と一致するため、再生ボタンの中央寄せ・読み込みボタンの右端揃えが
// それぞれのプレビューに合う。
fn render_controls_row(
    ui: &mut egui::Ui,
    deck_a: &mut StreamDeck,
    deck_b: &mut StreamDeck,
    cookie_args: &[String],
    deck_w: f32,
    master_w: f32,
    gap: f32,
) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.vertical(|ui| {
            ui.set_width(deck_w);
            render_deck_controls(ui, deck_a, cookie_args);
        });
        // 中央（マスター幅）は空け、左右の操作UIをプレビュー直下に揃える。
        ui.add_space(gap + master_w + gap);
        ui.vertical(|ui| {
            ui.set_width(deck_w);
            render_deck_controls(ui, deck_b, cookie_args);
        });
    });
}

// 1 デッキ分のプレビュー（映像 or プレースホルダ）を描画する。
fn render_deck_preview(ui: &mut egui::Ui, deck: &StreamDeck, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let rounding = egui::CornerRadius::same(12);
    ui.painter().rect_filled(rect, rounding, egui::Color32::BLACK);

    if let Some(texture) = deck.texture.as_ref() {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter()
            .image(texture.id(), rect, uv, egui::Color32::WHITE);
    } else {
        let message = if deck.running { "読み込み中..." } else { "停止中" };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            message,
            egui::FontId::proportional(13.0),
            egui::Color32::from_rgb(120, 130, 150),
        );
    }

    // 左上にデッキ識別ラベル（A / B）を重ねる。
    draw_corner_badge(ui.painter(), rect, deck.label);
}

// マスタープレビュー。A を不透明で描画し、B をフェーダー値の不透明度で重ねて
// 線形クロスフェード（左端=A, 右端=B）を表示する。
fn render_master_preview(
    ui: &mut egui::Ui,
    deck_a: &StreamDeck,
    deck_b: &StreamDeck,
    fader: f32,
    size: egui::Vec2,
) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let rounding = egui::CornerRadius::same(10);
    let painter = ui.painter();
    painter.rect_filled(rect, rounding, egui::Color32::BLACK);

    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    // 下地: A を不透明で描画。
    if let Some(texture) = deck_a.texture.as_ref() {
        painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
    }
    // 上に B をフェーダー不透明度で合成。alpha=f なので結果は A*(1-f)+B*f。
    if let Some(texture) = deck_b.texture.as_ref() {
        let alpha = (fader.clamp(0.0, 1.0) * 255.0).round() as u8;
        painter.image(texture.id(), rect, uv, egui::Color32::from_white_alpha(alpha));
    }

    // 枠線でマスター出力であることを示す。
    painter.rect_stroke(
        rect,
        rounding,
        egui::Stroke::new(1.5, egui::Color32::from_rgb(56, 189, 248)),
        egui::StrokeKind::Inside,
    );
    draw_corner_badge(painter, rect, "MASTER");
}

// プレビュー左上の小さなラベルバッジを描く。
fn draw_corner_badge(painter: &egui::Painter, rect: egui::Rect, text: &str) {
    let pos = rect.left_top() + egui::vec2(6.0, 6.0);
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(226, 234, 248),
    );
    let bg = egui::Rect::from_min_size(pos, galley.size() + egui::vec2(10.0, 4.0));
    painter.rect_filled(
        bg,
        egui::CornerRadius::same(5),
        egui::Color32::from_rgba_unmultiplied(8, 14, 24, 170),
    );
    painter.galley(pos + egui::vec2(5.0, 2.0), galley, egui::Color32::WHITE);
}

// マスタークロスフェーダー（左端=A, 右端=B）。
fn render_master_fader(ui: &mut egui::Ui, fader: &mut f32) {
    let height = 22.0;
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());

    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            *fader = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        }
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    } else if response.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    }

    let track_y = rect.center().y;
    let track_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, track_y),
        egui::vec2(rect.width(), 6.0),
    );
    let rounding = egui::CornerRadius::same(3);
    ui.painter().rect_filled(
        track_rect,
        rounding,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 31),
    );

    // 中央の基準（センター）マーク。
    ui.painter().rect_filled(
        egui::Rect::from_center_size(egui::pos2(track_rect.center().x, track_y), egui::vec2(2.0, 12.0)),
        egui::CornerRadius::same(1),
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60),
    );

    // ハンドルは縦長の長方形。
    let handle_x = track_rect.left() + track_rect.width() * fader.clamp(0.0, 1.0);
    let handle_rect = egui::Rect::from_center_size(
        egui::pos2(handle_x, track_y),
        egui::vec2(6.0, height),
    );
    ui.painter().rect_filled(
        handle_rect,
        egui::CornerRadius::same(2),
        egui::Color32::from_rgb(125, 211, 252),
    );
}

// 1 デッキ分のフル操作UI（トランスポート＋読み込み＋シークバー＋時間＋エラー）。
fn render_deck_controls(ui: &mut egui::Ui, deck: &mut StreamDeck, cookie_args: &[String]) {
    render_deck_transport(ui, deck, cookie_args);
    ui.add_space(6.0);
    render_deck_seek_bar(ui, deck);

    if let Some(err) = &deck.error {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(err)
                .size(11.5)
                .color(egui::Color32::from_rgb(248, 113, 113)),
        );
    }
}

// 巻き戻し/再生・一時停止/早送り＋右端に読み込み(Enter)ボタン。
fn render_deck_transport(ui: &mut egui::Ui, deck: &mut StreamDeck, cookie_args: &[String]) {
    let button_size = egui::vec2(48.0, 36.0);
    let enter_size = egui::vec2(44.0, 36.0);
    let gap = 8.0;
    let content_width = button_size.x * 3.0 + gap * 2.0;
    let total_width = ui.available_width();
    let leading = ((total_width - content_width) * 0.5).max(0.0);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(leading);
        if icon_button(ui, TransportIcon::Rewind, button_size).clicked() {
            deck.seek_relative(-0.5);
        }
        ui.add_space(gap);
        let play_icon = if deck.is_active() {
            TransportIcon::Pause
        } else {
            TransportIcon::Play
        };
        if icon_button(ui, play_icon, button_size).clicked() {
            deck.toggle_pause();
        }
        ui.add_space(gap);
        if icon_button(ui, TransportIcon::Forward, button_size).clicked() {
            deck.seek_relative(0.5);
        }

        // 残りの領域を使って Enter（読み込み）ボタンを右端へ揃える。
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button(ui, TransportIcon::Enter, enter_size).clicked() {
                deck.start_stream(cookie_args.to_vec());
            }
        });
    });
}

// ドラッグでシーク可能なシークバーと時間表示を描画する。
fn render_deck_seek_bar(ui: &mut egui::Ui, deck: &mut StreamDeck) {
    let position = deck.current_position();
    let duration = deck.duration;
    let seekable = deck.running && !deck.media_urls.is_empty() && duration.is_some();

    // 時間表示をシークバーの上に配置する。総時間が確定するまではプレースホルダ。
    let label_position = deck.scrubbing.unwrap_or(position);
    let time_label = match duration {
        Some(dur) if deck.running => {
            format!("{} / {}", format_time(label_position), format_time(dur))
        }
        _ => "--:-- / --:--".to_string(),
    };
    ui.label(
        egui::RichText::new(time_label)
            .size(11.5)
            .color(egui::Color32::from_rgb(170, 180, 200)),
    );
    ui.add_space(4.0);

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
                    deck.scrubbing = Some(fraction as f64 * dur);
                }
            }
            if response.drag_stopped() || response.clicked() {
                commit_target = deck.scrubbing.take();
            }
        }
    }

    let display_position = deck.scrubbing.unwrap_or(position);
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
        deck.seek(target);
    }
}

enum TransportIcon {
    Rewind,
    Play,
    Pause,
    Forward,
    Enter,
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
        TransportIcon::Enter => draw_enter_mark(painter, center, color),
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

// Enter（リターン）マークを描く。右上から下→左へ折れて左向き矢じりで終わる矢印。
fn draw_enter_mark(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(2.0, color);
    let c = center;
    let top = egui::pos2(c.x + 6.0, c.y - 7.0);
    let corner = egui::pos2(c.x + 6.0, c.y + 2.0);
    let left = egui::pos2(c.x - 5.0, c.y + 2.0);
    painter.line_segment([top, corner], stroke);
    painter.line_segment([corner, left], stroke);

    // 左向きの矢じり。
    let tip = egui::pos2(c.x - 7.0, c.y + 2.0);
    let head = vec![
        tip,
        egui::pos2(c.x - 2.0, c.y - 2.0),
        egui::pos2(c.x - 2.0, c.y + 6.0),
    ];
    painter.add(egui::Shape::convex_polygon(head, color, egui::Stroke::NONE));
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
