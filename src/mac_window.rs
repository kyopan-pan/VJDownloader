#[cfg(target_os = "macos")]
mod imp {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;

    pub fn enable_mouse_move_events_for_all_windows() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };

        let app = NSApplication::sharedApplication(mtm);
        let windows = app.windows();
        for window in windows.to_vec() {
            if !window.acceptsMouseMovedEvents() {
                window.setAcceptsMouseMovedEvents(true);
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::enable_mouse_move_events_for_all_windows;

#[cfg(not(target_os = "macos"))]
pub fn enable_mouse_move_events_for_all_windows() {}
