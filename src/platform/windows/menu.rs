// Windows には macOS のアプリケーションメニューに相当する常設メニューが存在しないため、
// 設定・ログ・通信速度測定・ストリーム再生・変換の起動口はアプリ内 UI 側で用意する。
//
// フラグ受け渡しの仕組みは macOS 版と同一にしてある。UI がクリックで request_open_* を呼び、
// UI ループが take_open_* で回収するため、app 側の分岐（app.rs の take_open_* 呼び出し）は
// プラットフォームによる差分を持たない。
//
// TODO(windows): アプリ内メニュー UI を追加し、各ボタンから request_open_* を呼ぶ。
// UI の追加を伴うため仕様変更となり、docs/spec.md の更新が必要。

use std::sync::atomic::{AtomicBool, Ordering};

static OPEN_SETTINGS_REQUEST: AtomicBool = AtomicBool::new(false);
static OPEN_LOGS_REQUEST: AtomicBool = AtomicBool::new(false);
static OPEN_SPEED_TEST_REQUEST: AtomicBool = AtomicBool::new(false);
static OPEN_STREAM_REQUEST: AtomicBool = AtomicBool::new(false);
static OPEN_CONVERTER_REQUEST: AtomicBool = AtomicBool::new(false);

// ネイティブメニューを持たないため、インストール処理は存在しない。
pub fn install_settings_menu() {}

pub fn take_open_settings_request() -> bool {
    OPEN_SETTINGS_REQUEST.swap(false, Ordering::Relaxed)
}

pub fn take_open_logs_request() -> bool {
    OPEN_LOGS_REQUEST.swap(false, Ordering::Relaxed)
}

pub fn take_open_speed_test_request() -> bool {
    OPEN_SPEED_TEST_REQUEST.swap(false, Ordering::Relaxed)
}

pub fn take_open_stream_request() -> bool {
    OPEN_STREAM_REQUEST.swap(false, Ordering::Relaxed)
}

pub fn take_open_converter_request() -> bool {
    OPEN_CONVERTER_REQUEST.swap(false, Ordering::Relaxed)
}

// 以下はアプリ内メニュー UI から呼び出す想定の入口。
// UI を接続するまで呼び出し元が存在しないため、dead_code を明示的に許可する。

#[allow(dead_code)]
pub fn request_open_settings() {
    OPEN_SETTINGS_REQUEST.store(true, Ordering::Relaxed);
}

#[allow(dead_code)]
pub fn request_open_logs() {
    OPEN_LOGS_REQUEST.store(true, Ordering::Relaxed);
}

#[allow(dead_code)]
pub fn request_open_speed_test() {
    OPEN_SPEED_TEST_REQUEST.store(true, Ordering::Relaxed);
}

#[allow(dead_code)]
pub fn request_open_stream() {
    OPEN_STREAM_REQUEST.store(true, Ordering::Relaxed);
}

#[allow(dead_code)]
pub fn request_open_converter() {
    OPEN_CONVERTER_REQUEST.store(true, Ordering::Relaxed);
}
