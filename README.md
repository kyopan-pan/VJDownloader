# VJDownloader

[![Build macOS](https://github.com/kyopan-pan/VJDownloader/actions/workflows/build-macos.yml/badge.svg)](https://github.com/kyopan-pan/VJDownloader/actions/workflows/build-macos.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

クリップボードのURLから動画をダウンロードし、ローカルのMP4を管理・送出するVJ向けデスクトップアプリです。
リストからのドラッグ&ドロップでVDMXなどのVJソフトへ直接渡せます。

Rust + [eframe/egui](https://github.com/emilk/egui) 製の単一バイナリで、macOS と Windows に対応します。

## 主な機能

- **クリップボードから即ダウンロード** — URLをコピーして `Download` を押すだけ。内部で [yt-dlp](https://github.com/yt-dlp/yt-dlp) を実行します。
- **ダウンロード仕様を3種から選択** — `標準（H.264優先）` / `1080p上限` / `最高画質 + 変換`。
- **ネイティブなドラッグ&ドロップ送出** — 一覧の行をドラッグしてVJソフトのビンへドロップできます。
- **ローカル動画検索** — SQLiteインデックスによる高速検索。ファイル名と動画内のコメント/説明タグを対象に、
  日本語の表記ゆれ（NFKC・全角半角・カタカナ/ひらがな）を吸収します。
- **MP4変換** — WebM / MOV / MKV などをドロップしてH.264 MP4へ変換します。
- **ストリーム再生 + Syphon出力**（macOSのみ） — URLの映像をウィンドウ内で再生しつつ、Syphonサーバーとして他アプリへ配信します。
- **AnimeThemes.moe 専用パイプライン** — 直リンクの受信とMP4変換を並列実行します。
- **Bot対策の検出** — YouTubeのレート制限や待機を検知し、進捗パネルとログに表示します。
- **通信速度測定・ログ画面** — 回線速度の確認と、直近の動作ログの確認・コピーができます。

明確に動作を保証しているサイトは **YouTube** と **AnimeThemes.moe** の2つです。
yt-dlpが対応する他のサイトも動作する可能性はありますが、検証はしていません。

## 動作環境

| | macOS | Windows |
| --- | --- | --- |
| バージョン | macOS 13 Ventura 以降 | Windows 10 / 11 |
| CPU | Apple Silicon (arm64) のみ | x64 / ARM64 |
| ffmpeg | アプリに同梱 | 初回セットアップで自動取得 |
| Syphon出力 | 対応 | 非対応 |

> **Intel Mac は非対応です。** 同梱しているffmpegがarm64ビルドのため動作しません。

## インストール

### macOS

1. [Releases](https://github.com/kyopan-pan/VJDownloader/releases/latest) から `VJDownloader-<version>-macos-arm64.dmg` をダウンロードします。
2. DMGを開き、`VJDownloader.app` を `アプリケーション` フォルダへドラッグします。
3. 初回起動時はアプリを **右クリック（またはControl+クリック）して「開く」** を選びます。

Appleの公証（notarization）を受けていないため、ダブルクリックでは
「開発元を検証できないため開けません」と表示されます。右クリックからの起動、または
`システム設定 > プライバシーとセキュリティ` の「このまま開く」で許可してください。

それでも「壊れているため開けません」と表示される場合は、隔離属性を外してください。

```bash
xattr -dr com.apple.quarantine /Applications/VJDownloader.app
```

### Windows

ビルド済みバイナリは配布していません。[ソースからビルド](#ソースからビルド)してください。

## 初回セットアップ

初回起動時、必要な外部ツールが未導入なら初回セットアップ画面が開きます。
`自動セットアップ` を押すと、各ツールを `~/.vjdownloader/bin` へ取得します。

| ツール | 取得元 | 備考 |
| --- | --- | --- |
| yt-dlp | [yt-dlp](https://github.com/yt-dlp/yt-dlp) の最新リリース | 動画のダウンロードに使用 |
| Deno | [deno](https://github.com/denoland/deno) の最新リリース | yt-dlpのJavaScriptランタイムとして使用 |
| ffmpeg / ffprobe | macOS: アプリに同梱<br>Windows: [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) | 変換・再生・メタデータ取得に使用 |

取得は「一時フォルダへダウンロード → 内容を検証 → 本体を置き換え」の順で行うため、
失敗しても既存のバイナリは壊れません。

## 使い方

1. ブラウザなどで動画のURLをコピーします。
2. VJDownloaderの `Download` を押します（クリップボードの文字列をそのままURLとして扱います）。
3. 完了したファイルが一覧に並びます。行をドラッグしてVJソフトへドロップします。

ダウンロード中に `Stop` を押すとキャンセルします。

### サブ画面の開き方

| 画面 | macOS | Windows |
| --- | --- | --- |
| 設定 | `Cmd + ,` / Appメニュー `設定...` | 右クリックメニュー |
| ログ | `Cmd + L` / Appメニュー `ログ...` | 右クリックメニュー |
| 通信速度測定 | Appメニュー `通信速度測定...` | 右クリックメニュー |
| ストリーム再生 | Appメニュー `ストリーム再生...` | 右クリックメニュー |
| MP4変換 | Appメニュー `動画をMP4に変換...` | 右クリックメニュー |

### 設定

設定画面では次を変更できます。

- **出力先フォルダ** — 既定は `~/Movies/VJDL`。
- **ダウンロード仕様** — 3つの選択肢。各項目にカーソルを合わせると説明が出ます。
- **YouTube認証（ブラウザクッキー）** — Bot判定を受ける場合に、指定ブラウザのクッキーをyt-dlpへ渡します。
- **検索対象フォルダ** — ローカル動画検索の対象ルート（複数指定可）。

### データの保存場所

| パス | 内容 |
| --- | --- |
| `~/.vjdownloader/settings.properties` | 設定ファイル |
| `~/.vjdownloader/bin/` | yt-dlp / Deno / ffmpeg / ffprobe |
| `~/.vjdownloader/search_index.sqlite3` | 動画検索インデックス |
| `~/Movies/VJDL/`（既定） | ダウンロード先 |

アンインストールする際は、アプリ本体と `~/.vjdownloader` を削除してください。

## ソースからビルド

[Rust](https://rustup.rs/) の stable ツールチェーンが必要です。

```bash
git clone https://github.com/kyopan-pan/VJDownloader.git
cd VJDownloader
cargo build --release
```

生成物は `target/release/VJDownloader`（Windowsは `VJDownloader.exe`）です。

既定フィーチャーは `syphon`（macOS向けのSyphon出力）です。無効化する場合は `--no-default-features` を付けます。

Syphon出力を使うには、公式の [Syphon.framework](https://github.com/Syphon/Syphon-Framework) を実行時に読める場所へ置きます。

- `third_party/Syphon.framework`（リポジトリ直下）
- `/Library/Frameworks/Syphon.framework`（Syphon公式インストーラの配置先）
- 環境変数 `SYPHON_FRAMEWORK_DIR` / `SYPHON_FRAMEWORK_PATH` で指定した場所

フレームワークが見つからない場合もビルド・起動はでき、ストリーム再生画面に初期化エラーとして表示されます。

### 同梱ffmpegを更新する

macOS向けの `assets/bin/ffmpeg` / `ffprobe` は自前ビルドしたものを同梱しています。
更新するときは次を実行してください（Xcode Command Line Tools のみ必要）。

```bash
scripts/build-ffmpeg-macos.sh --install
```

FFmpeg の最新ソースからビルドし、デプロイメントターゲット・依存関係・GPL混入の有無と、
本アプリが実際に使う機能（VideoToolboxエンコード、タグ取得、WebMデコード、rawvideoパイプ、
audiotoolbox出力、https入力）を検証してから配置します。検証に失敗した場合は配置しません。
配置後は [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) のコミットハッシュも更新してください。

### 開発コマンド

```bash
cargo check                  # 型チェック
cargo test                   # テスト
cargo clippy --all-targets   # lint
cargo fmt                    # 整形
cargo run                    # 起動
```

## ドキュメント

- [docs/spec.md](docs/spec.md) — 仕様
- [docs/search.md](docs/search.md) — 動画検索インデックスの設計
- [AGENTS.md](AGENTS.md) — 開発時の取り決め

## ライセンス

[MIT License](LICENSE) です。

同梱している FFmpeg / FFprobe は、外部ライブラリを含まない LGPL-2.1-or-later ビルドを
独立した実行ファイルとしてサブプロセス起動しています。ビルド済みバイナリを再配布する場合は
LGPL のライセンス文とソース入手先を同伴させてください（リリース用の `.app` には自動で同梱されます）。
各サードパーティの著作権表示は [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) を参照してください。

なお、本アプリで各サイトから動画を取得する行為については、各サイトの利用規約および
お住まいの地域の法令を確認のうえ、自己責任でご利用ください。
