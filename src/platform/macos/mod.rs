// macOS 実装。AppKit / Carbon を直接呼ぶコードはこの配下に閉じ込める。
// この配下は target_os = "macos" のときのみコンパイルされるため、各ファイルに cfg は不要。

pub mod file_dialog;
pub mod input_source;
pub mod menu;
pub mod window;
