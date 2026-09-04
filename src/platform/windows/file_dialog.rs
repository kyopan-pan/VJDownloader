use std::path::{Path, PathBuf};

// TODO(windows): IFileDialog（FOS_PICKFOLDERS）または rfd クレートでフォルダ選択を実装する。
// 未実装のため、現状は常にキャンセル扱いとして None を返す。
pub fn choose_directory(_current: Option<&Path>) -> Option<PathBuf> {
    None
}
