// プラットフォーム依存実装の境界層。
//
// このモジュールの外側（download/ stream/ search_index/ ui など）はプラットフォーム差分を
// 持たない。OS 固有の API 呼び出しはすべて macos/ または windows/ 配下へ閉じ込め、
// ここで cfg によって取り込む実装を切り替える。
//
// - common/  : 両プラットフォーム実装が共有する型
// - macos/   : macOS 実装（AppKit / Carbon）
// - windows/ : Windows 実装
//
// cfg で除外された側のファイルはパースも型チェックもされずコンパイル対象から外れるため、
// macOS ビルドに Windows 実装の依存が混ざることはない（逆も同様）。

mod common;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
compile_error!("VJDownloaderはmacOSとWindowsのみ対応しています。");

// ビルドターゲットに応じて実装ディレクトリを差し替える。
#[cfg_attr(target_os = "macos", path = "macos/mod.rs")]
#[cfg_attr(target_os = "windows", path = "windows/mod.rs")]
mod imp;

// 公開APIはドメインごとに明示して再エクスポートする。
// ここに列挙した項目は macos / windows の両実装に存在することが強制されるため、
// 片方へ実装を足し忘れるとそのターゲットのビルドがコンパイルエラーになる。

pub mod file_dialog {
    pub use super::imp::file_dialog::choose_directory;
}

pub mod input_source {
    pub use super::common::input_mode::InputMode;
    pub use super::imp::input_source::current_mode;
}

pub mod menu {
    pub use super::imp::menu::{
        install_settings_menu, take_open_converter_request, take_open_logs_request,
        take_open_settings_request, take_open_speed_test_request, take_open_stream_request,
    };
}

pub mod window {
    pub use super::imp::window::{
        apply_app_icon_from_icns, enable_mouse_move_events_for_all_windows,
    };
}
