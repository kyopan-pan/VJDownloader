use drag::{DragItem, Image, Options};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use crate::download::{read_clipboard_url, run_download, DownloadEvent, ProgressUpdate};
use crate::fs_utils::{delete_download_file, load_mp4_files};
use crate::paths::default_download_dir;
use crate::settings::{load_cookie_args, load_download_dir_from_settings};
use crate::theme::apply_theme;
use crate::ui;

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 720.0])
            .with_min_inner_size([360.0, 640.0]),
        ..Default::default()
    };

    eframe::run_native(
        "VJ Downloader (Rust)",
        options,
        Box::new(|cc| Ok(Box::new(DownloaderApp::new(cc)))),
    )
}

pub struct DownloaderApp {
    pub(crate) download_dir: PathBuf,
    pub(crate) downloaded_files: Vec<PathBuf>,
    pub(crate) status: Vec<String>,
    pub(crate) download_in_progress: bool,
    pub(crate) progress_message: String,
    pub(crate) progress_value: f32,
    pub(crate) progress_visible: bool,
    pub(crate) download_active_flag: Arc<AtomicBool>,
    pub(crate) rx: Option<mpsc::Receiver<DownloadEvent>>,
    pub(crate) last_scan: Instant,
    pub(crate) refresh_needed: bool,
}

impl DownloaderApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let download_dir = load_download_dir_from_settings().unwrap_or_else(default_download_dir);

        Self {
            download_dir,
            downloaded_files: Vec::new(),
            status: vec!["Ready.".to_string()],
            download_in_progress: false,
            progress_message: "待機中...".to_string(),
            progress_value: 0.0,
            progress_visible: false,
            download_active_flag: Arc::new(AtomicBool::new(false)),
            rx: None,
            last_scan: Instant::now() - Duration::from_secs(5),
            refresh_needed: true,
        }
    }

    pub(crate) fn push_status(&mut self, message: impl Into<String>) {
        const MAX_LINES: usize = 200;
        let message = message.into();
        println!("{message}");
        self.status.push(message);
        if self.status.len() > MAX_LINES {
            self.status.drain(0..self.status.len().saturating_sub(MAX_LINES));
        }
    }

    pub(crate) fn start_download_from_clipboard(&mut self) {
        let url = match read_clipboard_url() {
            Ok(url) => url,
            Err(err) => {
                self.push_status(err);
                return;
            }
        };

        let output_dir = self.download_dir.clone();
        let cookie_args = load_cookie_args();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.download_in_progress = true;
        self.download_active_flag.store(true, Ordering::Relaxed);

        self.push_status(format!(
            "Downloading to {}",
            output_dir.to_string_lossy()
        ));

        let active_flag = self.download_active_flag.clone();
        thread::spawn(move || run_download(url, output_dir, cookie_args, tx, active_flag));
    }

    pub(crate) fn delete_download(&mut self, path: &Path) {
        match delete_download_file(path) {
            Ok(()) => {
                self.refresh_needed = true;
            }
            Err(err) => self.push_status(format!("削除に失敗しました: {err}")),
        }
    }

    pub(crate) fn start_native_drag(&mut self, frame: &eframe::Frame, path: &Path) {
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                self.push_status(format!("ドラッグ対象の取得に失敗しました: {err}"));
                return;
            }
        };

        let icon_path = match drag_preview_icon_path() {
            Some(path) => path,
            None => {
                self.push_status("ドラッグ用アイコンが見つかりません。".to_string());
                return;
            }
        };

        if let Err(err) = drag::start_drag(
            frame,
            DragItem::Files(vec![path]),
            Image::File(icon_path),
            |_result, _position| {},
            Options::default(),
        ) {
            self.push_status(format!("ドラッグ開始に失敗しました: {err}"));
        }
    }

    fn poll_download_events(&mut self) {
        let mut events = Vec::new();
        if let Some(rx) = self.rx.as_ref() {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }

        let mut done = None;
        for event in events {
            match event {
                DownloadEvent::Log(line) => self.push_status(line),
                DownloadEvent::File => {
                    self.refresh_needed = true;
                }
                DownloadEvent::Progress(update) => self.handle_progress_update(update),
                DownloadEvent::Done(result) => done = Some(result),
            }
        }

        if let Some(result) = done {
            match result {
                Ok(()) => self.push_status("Download completed."),
                Err(err) => self.push_status(format!("Download failed: {err}")),
            }
            self.download_in_progress = false;
            self.download_active_flag.store(false, Ordering::Relaxed);
            self.rx = None;
            self.refresh_needed = true;
        }
    }

    fn refresh_downloads_if_needed(&mut self) {
        if self.refresh_needed || self.last_scan.elapsed() >= Duration::from_secs(2) {
            self.downloaded_files = load_mp4_files(&self.download_dir);
            self.last_scan = Instant::now();
            self.refresh_needed = false;
        }
    }

    fn handle_progress_update(&mut self, update: ProgressUpdate) {
        if update.visible {
            self.progress_message = update.message;
            self.progress_value = update.progress;
            self.progress_visible = true;
        } else {
            self.progress_message = "待機中...".to_string();
            self.progress_value = 0.0;
            self.progress_visible = false;
        }
    }
}

impl eframe::App for DownloaderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_download_events();
        self.refresh_downloads_if_needed();
        ui::render(self, ctx, _frame);
    }
}

fn drag_preview_icon_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let path = PathBuf::from(
            "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericDocumentIcon.icns",
        );
        if path.exists() {
            return Some(path);
        }
    }
    None
}
