use eframe::egui;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use crate::app::DownloaderApp;
use crate::cursor::pointing;

const TEST_URL: &str = "https://speed.cloudflare.com/__down?bytes=50000000";

#[derive(Clone, Debug)]
pub struct SpeedTestResult {
    download_mbps: f64,
    downloaded_mb: f64,
    total_seconds: f64,
    connect_ms: f64,
}

#[derive(Clone, Debug)]
enum SpeedTestEvent {
    Finished(Result<SpeedTestResult, String>),
}

pub struct SpeedTestUiState {
    pub show_speed_test: bool,
    running: bool,
    result: Option<SpeedTestResult>,
    error: Option<String>,
    started_at: Option<Instant>,
    tx: mpsc::Sender<SpeedTestEvent>,
    rx: mpsc::Receiver<SpeedTestEvent>,
}

impl SpeedTestUiState {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            show_speed_test: false,
            running: false,
            result: None,
            error: None,
            started_at: None,
            tx,
            rx,
        }
    }

    pub fn open_speed_test(&mut self) {
        self.show_speed_test = true;
    }

    pub fn start_test(&mut self) {
        if self.running {
            return;
        }

        self.running = true;
        self.result = None;
        self.error = None;
        self.started_at = Some(Instant::now());

        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = run_download_speed_test();
            let _ = tx.send(SpeedTestEvent::Finished(result));
        });
    }

    pub fn poll_updates(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                SpeedTestEvent::Finished(result) => {
                    self.running = false;
                    self.started_at = None;
                    match result {
                        Ok(result) => {
                            self.result = Some(result);
                            self.error = None;
                        }
                        Err(err) => {
                            self.result = None;
                            self.error = Some(err);
                        }
                    }
                }
            }
        }
    }
}

impl Default for SpeedTestUiState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn render_speed_test_viewport(app: &mut DownloaderApp, ctx: &egui::Context) {
    if !app.speed_test_ui.show_speed_test {
        return;
    }

    let mut close_requested = false;
    let viewport_id = speed_test_viewport_id();
    let builder = egui::ViewportBuilder::default()
        .with_title("通信速度測定")
        .with_inner_size(egui::vec2(520.0, 360.0))
        .with_min_inner_size(egui::vec2(420.0, 300.0))
        .with_always_on_top();

    ctx.show_viewport_immediate(viewport_id, builder, |ctx, class| {
        if ctx.input(|i| i.viewport().close_requested()) {
            close_requested = true;
        }

        match class {
            egui::ViewportClass::EmbeddedWindow => {
                let mut open = true;
                egui::Window::new("通信速度測定")
                    .collapsible(false)
                    .resizable(true)
                    .default_width(500.0)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        render_speed_test_contents(ui, app, ctx);
                    });
                if !open {
                    close_requested = true;
                }
            }
            _ => {
                let content_ctx = ctx.clone();
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_speed_test_contents(ui, app, &content_ctx);
                });
            }
        }
    });

    if close_requested {
        app.speed_test_ui.show_speed_test = false;
    }
}

fn render_speed_test_contents(ui: &mut egui::Ui, app: &mut DownloaderApp, ctx: &egui::Context) {
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 14,
            bottom: 16,
        })
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("通信速度測定")
                    .size(18.0)
                    .strong()
                    .color(egui::Color32::from_rgb(220, 230, 245)),
            );
            ui.label(
                egui::RichText::new(
                    "ダウンロード速度と接続時間を測定します。測定中は一時的に通信量が発生します。",
                )
                .size(12.0)
                .color(egui::Color32::from_rgb(140, 150, 170)),
            );
            ui.add_space(14.0);

            render_result_panel(ui, app, ctx);
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                let label = if app.speed_test_ui.running {
                    "測定中..."
                } else {
                    "測定開始"
                };
                let button = egui::Button::new(
                    egui::RichText::new(label)
                        .size(13.0)
                        .color(egui::Color32::from_rgb(8, 14, 24)),
                )
                .fill(if app.speed_test_ui.running {
                    egui::Color32::from_rgb(148, 163, 184)
                } else {
                    egui::Color32::from_rgb(56, 189, 248)
                })
                .corner_radius(egui::CornerRadius::same(12));

                if pointing(ui.add_enabled(!app.speed_test_ui.running, button)).clicked() {
                    app.speed_test_ui.start_test();
                }
            });
        });
}

fn render_result_panel(ui: &mut egui::Ui, app: &DownloaderApp, ctx: &egui::Context) {
    egui::Frame::NONE
        .fill(egui::Color32::from_rgb(20, 26, 40))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 56, 78)))
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(14, 14))
        .show(ui, |ui| {
            if app.speed_test_ui.running {
                let elapsed = app
                    .speed_test_ui
                    .started_at
                    .map(|started| started.elapsed().as_secs_f32())
                    .unwrap_or(0.0);
                ui.label(
                    egui::RichText::new(format!("測定しています... {elapsed:.1}s"))
                        .size(13.0)
                        .color(egui::Color32::from_rgb(226, 232, 240)),
                );
                ui.add_space(10.0);
                render_indeterminate_bar(ui, ctx);
                return;
            }

            if let Some(err) = &app.speed_test_ui.error {
                ui.label(
                    egui::RichText::new(err)
                        .size(12.5)
                        .color(egui::Color32::from_rgb(248, 113, 113)),
                );
                return;
            }

            let Some(result) = app.speed_test_ui.result.as_ref() else {
                ui.label(
                    egui::RichText::new("まだ測定していません。")
                        .size(12.5)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
                return;
            };

            ui.label(
                egui::RichText::new(format!("{:.1} Mbps", result.download_mbps))
                    .size(36.0)
                    .strong()
                    .color(egui::Color32::from_rgb(125, 211, 252)),
            );
            ui.add_space(8.0);
            egui::Grid::new("speed-test-result-grid")
                .num_columns(2)
                .spacing(egui::vec2(18.0, 8.0))
                .show(ui, |ui| {
                    result_row(
                        ui,
                        "取得データ量",
                        format!("{:.1} MB", result.downloaded_mb),
                    );
                    result_row(ui, "測定時間", format!("{:.2} 秒", result.total_seconds));
                    result_row(ui, "接続時間", format!("{:.0} ms", result.connect_ms));
                });
        });
}

fn result_row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(
        egui::RichText::new(label)
            .size(12.0)
            .color(egui::Color32::from_rgb(148, 163, 184)),
    );
    ui.label(
        egui::RichText::new(value)
            .size(12.0)
            .color(egui::Color32::from_rgb(226, 232, 240)),
    );
    ui.end_row();
}

fn render_indeterminate_bar(ui: &mut egui::Ui, ctx: &egui::Context) {
    let bar_height = 12.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), bar_height),
        egui::Sense::hover(),
    );
    let rounding = egui::CornerRadius::same(8);
    ui.painter().rect_filled(
        rect,
        rounding,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
    );

    let t = ctx.input(|input| input.time) as f32;
    let segment_fraction = 0.32f32;
    let phase = (t * 0.7) % 1.0;
    let start = phase * (1.0 + segment_fraction) - segment_fraction;
    let end = start + segment_fraction;
    let seg_min = (rect.left() + rect.width() * start).max(rect.left());
    let seg_max = (rect.left() + rect.width() * end).min(rect.right());
    if seg_max > seg_min {
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(seg_min, rect.top()),
                egui::pos2(seg_max, rect.bottom()),
            ),
            rounding,
            egui::Color32::from_rgb(56, 189, 248),
        );
    }
    ctx.request_repaint();
}

fn run_download_speed_test() -> Result<SpeedTestResult, String> {
    let output = Command::new("curl")
        .arg("-L")
        .arg("--silent")
        .arg("--show-error")
        .arg("--output")
        .arg("/dev/null")
        .arg("--max-time")
        .arg("20")
        .arg("--connect-timeout")
        .arg("8")
        .arg("--write-out")
        .arg("%{speed_download}\n%{time_total}\n%{size_download}\n%{time_connect}\n")
        .arg(TEST_URL)
        .output()
        .map_err(|err| format!("curl起動に失敗しました: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            output.status.to_string()
        } else {
            stderr
        };
        return Err(format!("速度測定に失敗しました: {detail}"));
    }

    parse_curl_result(&String::from_utf8_lossy(&output.stdout))
}

fn parse_curl_result(stdout: &str) -> Result<SpeedTestResult, String> {
    let mut lines = stdout.lines();
    let speed_bytes_per_second = parse_f64(lines.next(), "ダウンロード速度")?;
    let total_seconds = parse_f64(lines.next(), "測定時間")?;
    let downloaded_bytes = parse_f64(lines.next(), "取得データ量")?;
    let connect_seconds = parse_f64(lines.next(), "接続時間")?;

    if speed_bytes_per_second <= 0.0 || total_seconds <= 0.0 || downloaded_bytes <= 0.0 {
        return Err("測定結果が不正でした。ネットワーク状態を確認してください。".to_string());
    }

    Ok(SpeedTestResult {
        download_mbps: speed_bytes_per_second * 8.0 / 1_000_000.0,
        downloaded_mb: downloaded_bytes / 1_000_000.0,
        total_seconds,
        connect_ms: connect_seconds * 1000.0,
    })
}

fn parse_f64(value: Option<&str>, label: &str) -> Result<f64, String> {
    value
        .ok_or_else(|| format!("{label}を取得できませんでした。"))?
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("{label}の解析に失敗しました。"))
}

fn speed_test_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("speed_test_viewport")
}

#[cfg(test)]
mod tests {
    use super::parse_curl_result;

    #[test]
    fn parses_curl_speed_result() {
        let result = parse_curl_result("12500000\n4.0\n50000000\n0.123\n").unwrap();

        assert_eq!(result.download_mbps, 100.0);
        assert_eq!(result.downloaded_mb, 50.0);
        assert_eq!(result.total_seconds, 4.0);
        assert_eq!(result.connect_ms, 123.0);
    }

    #[test]
    fn rejects_empty_speed_result() {
        let err = parse_curl_result("0\n0\n0\n0\n").unwrap_err();

        assert!(err.contains("測定結果が不正"));
    }
}
