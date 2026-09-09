// Syphon.framework は実行時に dlopen する。
// ビルド時リンクにすると、フレームワーク未配置の環境ではアプリ自体をビルドできず、
// UI 上で原因を表示できないため、ここでは環境変数の変更だけを監視する。
fn main() {
    if std::env::var_os("CARGO_FEATURE_SYPHON").is_some() {
        println!("cargo:rerun-if-env-changed=SYPHON_FRAMEWORK_DIR");
        println!("cargo:rerun-if-env-changed=SYPHON_FRAMEWORK_PATH");
    }

    embed_windows_resource();
}

/// exe へアイコンとバージョン情報のリソースを埋め込む。
/// エクスプローラやタスクバーが参照するのは exe 内のリソースで、
/// macOS の `.icns` のような外付けの指定手段が Windows には無い。
#[cfg(windows)]
fn embed_windows_resource() {
    // ホストが Windows でもターゲットが違う場合（例: WSL 連携）は何もしない。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    println!("cargo:rerun-if-changed=assets/icon/App.ico");

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/icon/App.ico");
    // アイコンが欠けた配布物を気付かず公開しないよう、失敗はビルドエラーにする。
    resource
        .compile()
        .expect("Windowsリソース（アイコン）の埋め込みに失敗しました");
}

/// Windows 以外のホストでは埋め込み処理そのものが不要。
#[cfg(not(windows))]
fn embed_windows_resource() {}
