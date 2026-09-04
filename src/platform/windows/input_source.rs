use crate::platform::common::input_mode::InputMode;

// TODO(windows): GetKeyboardLayout と ImmGetConversionStatus で IME の変換モードを判定する。
// 未実装のため判定不能として None を返す。呼び出し側は None のとき状態変化を通知しない。
pub fn current_mode() -> Option<InputMode> {
    None
}
