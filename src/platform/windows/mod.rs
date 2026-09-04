// Windows 実装。Win32 API（COM / IMM32）を直接呼ぶコードはこの配下に閉じ込める。
// この配下は target_os = "windows" のときのみコンパイルされるため、各ファイルに cfg は不要。
//
// 実装状況:
// - file_dialog  : 実装済み（IFileOpenDialog）
// - input_source : 実装済み（GetKeyboardLayout + IMM32）
// - menu         : フラグ受け渡しのみ実装。アプリ内メニュー UI の追加が未完（TODO(windows) 参照）
// - window       : Windows では処理不要のため恒久的に no-op

pub mod file_dialog;
pub mod input_source;
pub mod menu;
pub mod window;
