#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputMode {
    Japanese,
    English,
    Other(String),
}

#[cfg(target_os = "macos")]
mod imp {
    use super::InputMode;
    use std::ffi::{CStr, c_char, c_void};

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCopyCurrentKeyboardInputSource() -> *const c_void;
        fn TISGetInputSourceProperty(input_source: *const c_void, key: *const c_void) -> *const c_void;

        static kTISPropertyInputSourceID: *const c_void;
        static kTISPropertyLocalizedName: *const c_void;
        static kTISPropertyInputSourceIsASCIICapable: *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
        fn CFGetTypeID(cf: *const c_void) -> usize;

        fn CFStringGetTypeID() -> usize;
        fn CFStringGetCStringPtr(string: *const c_void, encoding: u32) -> *const c_char;
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;

        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(boolean: *const c_void) -> u8;
    }

    pub fn current_mode() -> Option<InputMode> {
        let source = unsafe { TISCopyCurrentKeyboardInputSource() };
        if source.is_null() {
            return None;
        }

        let input_source_id = unsafe {
            cf_string_to_rust(TISGetInputSourceProperty(
                source,
                kTISPropertyInputSourceID,
            ))
        }
        .unwrap_or_default();

        let localized_name = unsafe {
            cf_string_to_rust(TISGetInputSourceProperty(
                source,
                kTISPropertyLocalizedName,
            ))
        }
        .unwrap_or_default();

        let ascii_capable = unsafe {
            cf_bool_to_rust(TISGetInputSourceProperty(
                source,
                kTISPropertyInputSourceIsASCIICapable,
            ))
        }
        .unwrap_or(false);

        unsafe { CFRelease(source) };

        let source_id_lower = input_source_id.to_lowercase();
        let name_lower = localized_name.to_lowercase();

        if source_id_lower.contains("kotoeri")
            || source_id_lower.contains("japanese")
            || localized_name.contains("日本語")
            || name_lower.contains("hiragana")
            || name_lower.contains("katakana")
        {
            return Some(InputMode::Japanese);
        }

        if source_id_lower.contains(".abc")
            || localized_name == "ABC"
            || (ascii_capable && source_id_lower.contains(".us"))
            || (ascii_capable && name_lower == "us")
        {
            return Some(InputMode::English);
        }

        let display = if !localized_name.is_empty() {
            localized_name
        } else if !input_source_id.is_empty() {
            input_source_id
        } else {
            "unknown".to_string()
        };

        Some(InputMode::Other(display))
    }

    unsafe fn cf_string_to_rust(cf: *const c_void) -> Option<String> {
        if cf.is_null() {
            return None;
        }
        if unsafe { CFGetTypeID(cf) } != unsafe { CFStringGetTypeID() } {
            return None;
        }

        let ptr = unsafe { CFStringGetCStringPtr(cf, K_CF_STRING_ENCODING_UTF8) };
        if !ptr.is_null() {
            return unsafe { CStr::from_ptr(ptr) }
                .to_str()
                .ok()
                .map(str::to_owned);
        }

        let mut buffer = vec![0 as c_char; 1024];
        let ok = unsafe {
            CFStringGetCString(
                cf,
                buffer.as_mut_ptr(),
                buffer.len() as isize,
                K_CF_STRING_ENCODING_UTF8,
            )
        };
        if ok == 0 {
            return None;
        }

        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_str()
            .ok()
            .map(str::to_owned)
    }

    unsafe fn cf_bool_to_rust(cf: *const c_void) -> Option<bool> {
        if cf.is_null() {
            return None;
        }
        if unsafe { CFGetTypeID(cf) } != unsafe { CFBooleanGetTypeID() } {
            return None;
        }
        Some(unsafe { CFBooleanGetValue(cf) != 0 })
    }
}

#[cfg(target_os = "macos")]
pub use imp::current_mode;

#[cfg(not(target_os = "macos"))]
pub fn current_mode() -> Option<InputMode> {
    None
}
