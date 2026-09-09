# ビルドと配布

アプリの振る舞いそのものは `docs/spec.md`、検索エンジンの設計は `docs/search.md` を参照する。
このファイルはビルド構成・CI・リリース成果物だけを扱う。

## ビルド構成

- 既定フィーチャーは `syphon`。
- Windowsのビルドは`syphon`フィーチャーを無効化する（Syphon出力はmacOS限定のため）。
- Windowsのリリースビルドはコンソールウィンドウを出さない。この挙動は`docs/spec.md`の
  「プラットフォーム依存実装の分離」に記載する。

## 検証

- `cargo check`は実行中のOS側のみを検証する。もう一方のターゲットの検証はCIまたは実機で行う。
- 残作業は各ファイルの`TODO(windows)`に記載する。

## CI

- CIはmacOS（arm64）とWindows（x64）の両方をビルドする。配布物はmacOSが`.app`入りのDMG、Windowsが`.exe`とライセンス文書を入れたZIP。
- ワークフローは`.github/workflows/build-macos.yml`と`.github/workflows/build-windows.yml`。

## リリース

- `.github/workflows/release.yml`がバージョンタグの push で公開する。タグと`Cargo.toml`の`version`が一致しない場合は失敗する。
- リリースには、バージョン付きの配布物（`VJDownloader-<version>-macos-arm64.dmg` / `VJDownloader-<version>-windows-x64.zip`）と、内容が同一でバージョンを含まない別名（`VJDownloader-macos-arm64.dmg` / `VJDownloader-windows-x64.zip`）の両方を添付する。別名は`releases/latest/download/<固定名>`でREADMEから最新版へ直リンクするために必要で、バージョン付きは過去版の識別用に残す。
