// Windows には macOS のアプリケーションメニューに相当する常設メニューがないため、
// 設定・ログ・通信速度測定・ストリーム再生・変換の起動口はアプリ内 UI として用意する必要がある。
//
// TODO(windows): アプリ内メニュー（ツールバー等）からのリクエストを
// ここのフラグ経由で app 側へ渡す形に接続する。UI 側の追加を伴うため仕様変更となる。

pub fn install_settings_menu() {}

pub fn take_open_settings_request() -> bool {
    false
}

pub fn take_open_logs_request() -> bool {
    false
}

pub fn take_open_speed_test_request() -> bool {
    false
}

pub fn take_open_stream_request() -> bool {
    false
}

pub fn take_open_converter_request() -> bool {
    false
}
