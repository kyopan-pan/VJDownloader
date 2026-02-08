#[cfg(target_os = "macos")]
mod imp {
    use std::path::{Path, PathBuf};

    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{MainThreadMarker, NSString, NSURL};

    pub fn choose_directory(current: Option<&Path>) -> Option<PathBuf> {
        let mtm = MainThreadMarker::new()?;
        let panel = unsafe { NSOpenPanel::openPanel(mtm) };
        unsafe {
            panel.setCanChooseDirectories(true);
            panel.setCanChooseFiles(false);
            panel.setAllowsMultipleSelection(false);
        }

        if let Some(path) = current {
            if let Some(path_str) = path.to_str() {
                let ns_path = NSString::from_str(path_str);
                let url = unsafe { NSURL::fileURLWithPath_isDirectory(&ns_path, true) };
                unsafe {
                    panel.setDirectoryURL(Some(&url));
                }
            }
        }

        let response = unsafe { panel.runModal() };
        if response != NSModalResponseOK {
            return None;
        }

        let urls = unsafe { panel.URLs() };
        if urls.count() == 0 {
            return None;
        }

        let url = unsafe { urls.objectAtIndex(0) };
        let path_ns = unsafe { url.path() }?;
        Some(PathBuf::from(path_ns.to_string()))
    }
}

#[cfg(target_os = "macos")]
pub use imp::choose_directory;

#[cfg(not(target_os = "macos"))]
pub fn choose_directory(_current: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    None
}
