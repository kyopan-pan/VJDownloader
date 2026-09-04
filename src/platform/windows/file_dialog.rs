use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, IBindCtx,
};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, IUnknown};

// macOS の NSOpenPanel と同じく、フォルダのみを選択させるモーダルダイアログを開く。
pub fn choose_directory(current: Option<&Path>) -> Option<PathBuf> {
    // COM はダイアログ表示に必須。eframe のメインスレッドは既に STA 初期化済みの想定だが、
    // 二重初期化しても同一モードなら S_FALSE が返るだけで害はない。
    // 成功した場合のみ CoUninitialize を呼び、初期化回数と釣り合わせる。
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let selected = unsafe { show_folder_dialog(current) };
    if initialized {
        unsafe { CoUninitialize() };
    }
    selected
}

unsafe fn show_folder_dialog(current: Option<&Path>) -> Option<PathBuf> {
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None::<&IUnknown>, CLSCTX_INPROC_SERVER) }.ok()?;

    // フォルダのみ選択可・実在するファイルシステム上のパスに限定する。
    // 仮想フォルダ（ライブラリ等）を除外しないと GetDisplayName でパスを取得できない。
    let options = unsafe { dialog.GetOptions() }.ok()?;
    unsafe {
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)
    }
    .ok()?;

    if let Some(path) = current {
        if let Some(item) = unsafe { shell_item_from_path(path) } {
            // 初期表示位置の指定は失敗しても選択自体は続行させる。
            let _ = unsafe { dialog.SetFolder(&item) };
        }
    }

    // TODO(windows): 本来はオーナーウィンドウのHWNDを渡して正しくモーダル化すべきだが、
    // choose_directory はウィンドウハンドルを受け取らないため現状は None とする。
    // ユーザーがキャンセルした場合も Err（ERROR_CANCELLED）が返るため、選択なしとして扱う。
    unsafe { dialog.Show(None) }.ok()?;

    let item = unsafe { dialog.GetResult() }.ok()?;
    unsafe { file_system_path(&item) }
}

// 現在の保存先をダイアログの初期表示位置として渡すための IShellItem を作る。
unsafe fn shell_item_from_path(path: &Path) -> Option<IShellItem> {
    let path_str = path.to_str()?;
    let wide = HSTRING::from(path_str);
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(&wide, None::<&IBindCtx>) }.ok()?;
    Some(item)
}

// 選択された項目からファイルシステム上の絶対パスを取り出す。
unsafe fn file_system_path(item: &IShellItem) -> Option<PathBuf> {
    let raw = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()?;
    if raw.is_null() {
        return None;
    }

    let text = unsafe { raw.to_string() }.ok();
    // GetDisplayName が返す文字列は COM のタスクメモリ上にあるため、呼び出し側で解放する。
    unsafe { CoTaskMemFree(Some(raw.0 as *const c_void)) };

    Some(PathBuf::from(text?))
}
