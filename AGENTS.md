# AGENTS.md

## 概要

- VJDownloader は、クリップボードのURLから動画をダウンロードし、ローカルのMP4を管理・送出する VJ 向けデスクトップアプリ。
- Rust 2024 edition + eframe/egui のGUIアプリ。単一バイナリ `VJDownloader` をビルドする。
- 対応プラットフォームは macOS と Windows のみ。それ以外のターゲットは `compile_error!` でビルドを止める。
- yt-dlp / Deno / ffmpeg / ffprobe を外部ツールとして `~/.vjdownloader/bin` から実行する。未導入時は初回セットアップ画面で取得する。
- 明確に動作を保証しているサイトはYouTube/Animethemes.moeの2サイト。
- 仕様は `docs/spec.md`。検索エンジンの設計は `docs/search.md`。ビルド・CI・リリース成果物は `docs/release.md`。

## Commands

```bash
cargo check                      # 型チェック（既定フィーチャー: syphon 有効）
cargo test                       # 全テスト（43件）
cargo test search_index -- --test-threads=1   # 検索インデックスのみ直列実行
cargo clippy --all-targets       # lint
cargo fmt                        # 整形
cargo run                        # 起動（macOS）
```

- 既定フィーチャーは `syphon`。無効化する場合は `--no-default-features`。
- Windows ターゲットのツールチェーンは未導入。型チェックしたい場合は `rustup target add x86_64-pc-windows-msvc` を追加してから `cargo check --target x86_64-pc-windows-msvc` を使う（リンクは不可）。
- CI は macOS（`.github/workflows/build-macos.yml`）と Windows（`.github/workflows/build-windows.yml`）をそれぞれビルドする。Windows は x64 のみで、`--no-default-features`（syphon 無効）でビルドする。実際の動作確認は実機で行う。
- リリースは `.github/workflows/release.yml` がバージョンタグ（`v*` または `0.0.0` 形式）の push で公開する。macOS の DMG と Windows の ZIP を別ジョブでビルドし、両方揃ってから `publish` ジョブが1つのリリースへ添付する。タグと `Cargo.toml` の `version` が一致しない場合はワークフローが失敗する。

## 完了条件

実装を終えたら、以下をすべて満たすこと。

1. `cargo check` がエラー・警告なしで完了する。
2. `cargo test` が全件成功する（失敗0件）。
3. `cargo clippy --all-targets` の警告を新たに増やしていない。既存の警告は `too_many_arguments` の7件のみで、引数を構造体へまとめる改修が必要なため保留している。変更箇所に出た警告は解消する。
4. `cargo fmt --check` が差分なしで完了する。
5. 仕様に影響する変更なら `docs/spec.md` を更新済みである。

`cargo check` で警告が出た場合は、依存クレートの最新情報をWebで確認し、アプリ構造への大きな変更を伴わない範囲で修正する。

## 境界

### 必ず行う

- OS依存の実装は `src/platform/` 配下に閉じる。呼び出し側に `#[cfg(target_os = ...)]` を散らさない。
- 外部ツール（yt-dlp / Deno / ffmpeg）のパス解決は `src/paths.rs` の関数を経由する。パスを直書きしない。
- 設定キーを追加したら `src/settings/mod.rs` の読み書きと `docs/spec.md` の両方を更新する。

### 事前に確認する

- 依存クレートの追加、およびバージョン更新。
- `.github/workflows/` の変更。
- 既存の設定キー名・保存形式の変更（ユーザーの既存設定ファイルとの互換性が壊れる）。
- モジュール分割や大規模なリファクタリング。
- `git commit` / `git push`。

### 禁止事項

- `assets/bin/ffmpeg` / `assets/bin/ffprobe` の変更・再生成。macOS向け同梱バイナリで、`src/bundled.rs` が `include_bytes!` で埋め込む。`.gitattributes` で binary 指定しており、改行変換が入ると壊れる。再生成が必要な場合は `scripts/build-ffmpeg-macos.sh --install` を使う（外部ライブラリなしのLGPLビルド）。GPL版へ差し替えると配布物のライセンス条件が変わるため `THIRD_PARTY_NOTICES.md` の見直しが必要。
- `Cargo.lock` の手編集。`cargo` コマンド経由でのみ更新する。
- `target/` `third_party/` `syphon-src/` への手動配置。ビルド生成物およびCI生成物。
- `.idea/` の編集。

## コード構成

- `src/main.rs`: エントリポイント。モジュール宣言と Windows のサブシステム切り替え。
- `src/app.rs`: アプリ本体。`eframe::App` 実装とサブ画面の起動要求の回収。
- `src/ui.rs` / `src/theme.rs` / `src/cursor.rs`: メイン画面の描画、ダークテーマ、カーソル制御。
- `src/paths.rs`: `~/.vjdownloader` 配下の内部パス解決（設定ファイル・bin・検索DB）。
- `src/fs_utils.rs`: ディレクトリ作成、MP4一覧取得、削除、実行権限判定。
- `src/bundled.rs`: 同梱 ffmpeg/ffprobe の配置。macOS のみバイナリを埋め込み、Windows は探索のみ。
- `src/settings/`: `~/.vjdownloader/settings.properties` の読み書き（`mod.rs`）と設定画面（`ui.rs`）。
- `src/download/`: yt-dlp 実行（`process.rs`）、外部ツールの取得と置き換え（`tools.rs`）、Bot対策検出（`guard.rs`）、AnimeThemes連携（`animethemes.rs`）。
- `src/search_index/`: SQLite + notify によるローカル動画検索。設計は `docs/search.md`。
- `src/stream/`: ストリーム再生画面と Syphon 出力（`syphon` フィーチャー時のみ、macOS限定）。
- `src/converter.rs`: MP4変換画面。H.264エンコーダは macOS が `h264_videotoolbox`、Windows が `libx264`。
- `src/speed_test/`: 通信速度測定画面。
- `src/logs/`: ログ収集と表示画面。
- `src/platform/`: OS依存実装の境界層。`common/` に境界をまたぐ共通型、`macos/` と `windows/` に各実装。配下は各OSでのみコンパイルされるため、ファイル内に `cfg` は不要。

## コードスタイル

- コード内コメント、ドキュメント、コミットメッセージは日本語で書く。
- コメントは「何をしているか」ではなく「なぜそうしているか」を書く。既存コードの密度に合わせる。
- サブ画面は `show_viewport_deferred` による独立ウィンドウとして実装する。

## ドキュメント運用

- 仕様を追加・変更したら `docs/spec.md` の該当箇所を追記・更新する。仕様を削除したら該当記述も削除する。
- `docs/spec.md` はプラットフォーム差分がある項目に macOS / Windows を明記する。
- `docs/spec.md` はアプリの振る舞いだけを書く。設計判断や実装ファイルの案内は `docs/search.md`、
  ビルド構成・CI・配布物は `docs/release.md` に置き、同じ内容を二重に書かない。
- ドキュメント内のファイル参照はリポジトリルートからの相対パスで書く。ローカルの絶対パスを書かない。

## コミット / PR

- コミットメッセージは Conventional Commits に従う。1行目は `<型>: <要約>` 形式（コロン + 半角スペース）。
- 型は `feat` / `fix` / `docs` / `style` / `refactor` / `perf` / `test` / `build` / `ci` / `chore` から選ぶ。
- 要約は日本語で書き、末尾に句点を付けない。例: `feat: Windows向けffmpeg/ffprobeを初回セットアップで取得`
- 影響範囲を示す場合はスコープを付ける。例: `fix(search): カタカナ検索の正規化漏れを修正`
- 破壊的変更は型の後に `!` を付ける。例: `feat!: 設定キーの保存形式を変更`
- 過去の履歴には `feat.` / `Mod.` / `Add.` / `Del.` 形式が混在するが、新規コミットでは使わない。
- ブランチ運用は GitHub Flow。main から `feat/xxx` を切り、PR経由で main へマージする。
- main への push で macOS ビルドが走るため、ビルドを壊す変更を main へ直接入れない。
