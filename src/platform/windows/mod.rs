// Windows 実装。
//
// 現時点では公開APIの形だけを揃えた未実装スタブであり、中身は macOS 版と同じ
// 「何もしない」挙動（移設前の `not(target_os = "macos")` スタブと同等）である。
// 各ファイルの TODO(windows) が実装すべき内容を示す。
//
// この配下は target_os = "windows" のときのみコンパイルされるため、各ファイルに cfg は不要。

pub mod file_dialog;
pub mod input_source;
pub mod menu;
pub mod window;
