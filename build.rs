// Syphon.framework は実行時に dlopen する。
// ビルド時リンクにすると、フレームワーク未配置の環境ではアプリ自体をビルドできず、
// UI 上で原因を表示できないため、ここでは環境変数の変更だけを監視する。
fn main() {
    if std::env::var_os("CARGO_FEATURE_SYPHON").is_some() {
        println!("cargo:rerun-if-env-changed=SYPHON_FRAMEWORK_DIR");
        println!("cargo:rerun-if-env-changed=SYPHON_FRAMEWORK_PATH");
    }
}
