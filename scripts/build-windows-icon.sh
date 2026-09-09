#!/bin/bash
# assets/icon/App.ico を assets/icon/App.iconset の PNG から生成する。
#
# Windows は exe へ埋め込んだ ICO しか見ないため、macOS 用の .icns とは別に
# マルチ解像度の .ico が要る。ImageMagick 等の外部ツールに依存しないよう、
# 一時的な Cargo プロジェクトを作って image クレートで変換する。
#
# 使い方:
#   scripts/build-windows-icon.sh            # 生成して assets/icon/App.ico を更新
#
# 必要なもの: Rust ツールチェーン（cargo）のみ

set -euo pipefail

readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly ICONSET="${REPO_ROOT}/assets/icon/App.iconset"
readonly OUT="${REPO_ROOT}/assets/icon/App.ico"
readonly WORK_DIR="${TMPDIR:-/tmp}/vjdl-icon-build"

[ -d "${ICONSET}" ] || { echo "iconsetが見つかりません: ${ICONSET}" >&2; exit 1; }

echo "==> 変換ツールを用意: ${WORK_DIR}"
mkdir -p "${WORK_DIR}/src"

cat > "${WORK_DIR}/Cargo.toml" << 'EOF'
[package]
name = "vjdl-icogen"
version = "0.0.0"
edition = "2021"

[dependencies]
image = { version = "0.25.9", default-features = false, features = ["png", "ico"] }
EOF

cat > "${WORK_DIR}/src/main.rs" << 'EOF'
use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::imageops::FilterType;
use image::{ExtendedColorType, GenericImageView};
use std::fs::File;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let iconset = &args[1];
    let out = &args[2];

    // Windows が参照する解像度を一通り入れる。実寸の素材がある解像度はそのまま使い、
    // 素材の無い 24/48 のみ 512px から縮小する。
    let sizes: [(u32, &str); 7] = [
        (16, "icon_16x16.png"),
        (24, "icon_512x512.png"),
        (32, "icon_32x32.png"),
        (48, "icon_512x512.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_256x256.png"),
    ];

    let mut frames = Vec::new();
    for (size, file) in sizes {
        let source = format!("{iconset}/{file}");
        let image = image::open(&source).unwrap_or_else(|e| panic!("{source}: {e}"));
        let image = if image.dimensions() == (size, size) {
            image
        } else {
            image.resize_exact(size, size, FilterType::Lanczos3)
        };
        let rgba = image.into_rgba8();
        // 各エントリを PNG で持つ形式。Windows Vista 以降はどの解像度でも解釈できる。
        frames.push(
            IcoFrame::as_png(rgba.as_raw(), size, size, ExtendedColorType::Rgba8)
                .expect("ICOフレームの生成に失敗"),
        );
        println!("  {size}px <- {file}");
    }

    IcoEncoder::new(File::create(out).expect("出力ファイルを作成できません"))
        .encode_images(&frames)
        .expect("ICOの書き出しに失敗");
}
EOF

echo "==> 変換"
(cd "${WORK_DIR}" && cargo run --release --quiet -- "${ICONSET}" "${OUT}")

echo "==> 完了: ${OUT}"
ls -l "${OUT}"
