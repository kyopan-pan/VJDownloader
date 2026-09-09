// 子プロセス起動のプラットフォーム差分。
// macOS では GUI アプリから子プロセスを起動しても端末は開かないため、追加の設定は不要。

use std::ffi::OsStr;
use std::process::Command;

/// コンソールウィンドウを開かずに子プロセスを起動する `Command` を作る。
/// macOS では `Command::new` と等価。
pub fn hidden_command<S: AsRef<OsStr>>(program: S) -> Command {
    Command::new(program)
}
