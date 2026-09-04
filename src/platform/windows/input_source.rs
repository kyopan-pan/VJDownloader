use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::Ime::{
    IME_CMODE_NATIVE, IME_CONVERSION_MODE, ImmGetContext, ImmGetConversionStatus, ImmReleaseContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::platform::common::input_mode::InputMode;

// LANGID の下位10bitがプライマリ言語ID。
const PRIMARY_LANGUAGE_MASK: u16 = 0x03FF;
const LANG_CHINESE: u16 = 0x04;
const LANG_JAPANESE: u16 = 0x11;
const LANG_KOREAN: u16 = 0x12;

// 前面ウィンドウのキーボードレイアウトとIMEの変換モードから入力状態を判定する。
// macOS 版が入力ソースをシステム全体から取得するのと同様、対象は前面ウィンドウとする。
pub fn current_mode() -> Option<InputMode> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }

    let thread_id = unsafe { GetWindowThreadProcessId(hwnd, None) };
    if thread_id == 0 {
        return None;
    }

    let hkl = unsafe { GetKeyboardLayout(thread_id) };
    // HKL の下位ワードが入力ロケールの LANGID。
    let language_id = (hkl.0 as usize & 0xFFFF) as u16;

    match language_id & PRIMARY_LANGUAGE_MASK {
        LANG_JAPANESE => Some(japanese_layout_mode(hwnd)),
        // 日本語以外のIMEを持つレイアウトは、かな／英字のどちらとも言えないため個別に通知する。
        LANG_KOREAN => Some(InputMode::Other("韓国語".to_string())),
        LANG_CHINESE => Some(InputMode::Other("中国語".to_string())),
        // それ以外のレイアウトはIMEを伴わない直接入力とみなす。
        _ => Some(InputMode::English),
    }
}

// 日本語レイアウト時に、IMEがかな入力状態か英数入力状態かを判定する。
fn japanese_layout_mode(hwnd: HWND) -> InputMode {
    let context = unsafe { ImmGetContext(hwnd) };
    if context.0.is_null() {
        // IMEが無効化されているウィンドウは直接入力として扱う。
        return InputMode::English;
    }

    let mut conversion = IME_CONVERSION_MODE(0);
    let acquired =
        unsafe { ImmGetConversionStatus(context, Some(&mut conversion), None) }.as_bool();
    unsafe { ImmReleaseContext(hwnd, context) };

    if !acquired {
        return InputMode::English;
    }

    // IME_CMODE_NATIVE が立っていればかな入力、落ちていれば英数入力。
    if (conversion & IME_CMODE_NATIVE).0 != 0 {
        InputMode::Japanese
    } else {
        InputMode::English
    }
}
