#[cfg(target_os = "macos")]
mod imp {
    use objc2::AnyThread;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{MainThreadMarker, NSString};
    use std::path::PathBuf;

    pub fn apply_app_icon_from_icns() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };

        let Some(icon_path) = find_app_icon_path() else {
            return;
        };
        let Some(icon_path) = icon_path.to_str() else {
            return;
        };

        let app = NSApplication::sharedApplication(mtm);
        let file_name = NSString::from_str(icon_path);
        let Some(icon_image) = NSImage::initWithContentsOfFile(NSImage::alloc(), &file_name) else {
            return;
        };
        icon_image.setSize(objc2_foundation::NSSize::new(128.0, 128.0));
        unsafe { app.setApplicationIconImage(Some(&icon_image)) };
    }

    fn find_app_icon_path() -> Option<PathBuf> {
        let mut candidates = Vec::new();

        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(contents_dir) = exe_path.parent().and_then(|p| p.parent()) {
                candidates.push(contents_dir.join("Resources/App.icns"));
            }
            if let Some(project_root) = exe_path
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
            {
                candidates.push(project_root.join("assets/icon/App.icns"));
            }
        }

        if let Ok(current_dir) = std::env::current_dir() {
            candidates.push(current_dir.join("assets/icon/App.icns"));
        }

        candidates.into_iter().find(|path| path.is_file())
    }

    pub fn enable_mouse_move_events_for_all_windows(force: bool) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };

        let app = NSApplication::sharedApplication(mtm);
        let windows = app.windows();
        for window in windows.to_vec() {
            if force || !window.acceptsMouseMovedEvents() {
                window.setAcceptsMouseMovedEvents(true);
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::apply_app_icon_from_icns;
#[cfg(target_os = "macos")]
pub use imp::enable_mouse_move_events_for_all_windows;

#[cfg(not(target_os = "macos"))]
pub fn apply_app_icon_from_icns() {}

#[cfg(not(target_os = "macos"))]
pub fn enable_mouse_move_events_for_all_windows(_force: bool) {}
