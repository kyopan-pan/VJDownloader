// TODO(windows): アプリアイコンは .ico を embed-resource で実行ファイルへ埋め込む方式を採るため、
// 実行時に適用する処理は不要になる見込み。タスクバー表示の調整が必要になった場合はここへ実装する。
pub fn apply_app_icon_from_icns() {}

// Windows ではマウス移動イベントが既定で配送されるため、明示的な有効化は不要。
pub fn enable_mouse_move_events_for_all_windows(_force: bool) {}
