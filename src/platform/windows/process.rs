// 子プロセス起動のプラットフォーム差分。
//
// Windows はコンソールを持たない GUI プロセス（windows_subsystem = "windows"）から
// コンソールサブシステムの実行ファイル（yt-dlp.exe / ffmpeg.exe / curl.exe など）を起動すると、
// OS がその子プロセスへ新しいコンソールを割り当てる。結果として操作のたびに黒いウィンドウが
// 一瞬だけ開閉し、同時に複数プロセスを起動する処理では複数のウィンドウが現れる。
// これを防ぐため、アプリが起動する子プロセスにはすべて CREATE_NO_WINDOW を付ける。

use std::ffi::OsStr;
use std::os::windows::process::CommandExt;
use std::process::Command;

// CREATE_NO_WINDOW。windows クレートでは Win32_System_Threading フィーチャー配下にあり、
// この用途だけのためにフィーチャーを増やしたくないので値を直接持つ。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// コンソールウィンドウを開かずに子プロセスを起動する `Command` を作る。
pub fn hidden_command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}
