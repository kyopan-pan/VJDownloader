// `syphon` フィーチャー有効時に公式 Syphon.framework をリンクする。
//
// フレームワークの場所は環境変数 SYPHON_FRAMEWORK_DIR で指定できる。未指定時は
// リポジトリ同梱の `third_party/` を探索する。実行時に .app/Contents/Frameworks から
// 解決できるよう rpath を追加し、開発実行(cargo run)用に検索ディレクトリも rpath に加える。
fn main() {
    if std::env::var_os("CARGO_FEATURE_SYPHON").is_some() {
        let dir = std::env::var("SYPHON_FRAMEWORK_DIR")
            .unwrap_or_else(|_| format!("{}/third_party", env!("CARGO_MANIFEST_DIR")));

        println!("cargo:rerun-if-env-changed=SYPHON_FRAMEWORK_DIR");
        println!("cargo:rustc-link-search=framework={dir}");
        println!("cargo:rustc-link-lib=framework=Syphon");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
