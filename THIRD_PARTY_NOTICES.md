# サードパーティ ライセンス表示

VJDownloader 本体のソースコードは [MIT License](LICENSE) です。
本ドキュメントは、配布物に同梱されるもの・実行時に取得されるサードパーティ製ソフトウェアの
ライセンスと著作権表示をまとめたものです。

## 配布物に同梱されるもの

### FFmpeg / FFprobe（LGPL-2.1-or-later）

macOS向けのビルドは、`assets/bin/ffmpeg` と `assets/bin/ffprobe` を `include_bytes!` で
実行ファイルへ埋め込みます（`src/bundled.rs`）。

| 項目       | 内容                                                                                            |
|------------|-------------------------------------------------------------------------------------------------|
| バージョン | `git-2026-09-08-1de77bb`（FFmpeg upstream コミット `1de77bb8987e2c7364302c91b9f13958e419124e`） |
| 対象       | macOS 13.0以降 / arm64                                                                          |
| ライセンス | LGPL-2.1-or-later（[licenses/LGPL-2.1.txt](licenses/LGPL-2.1.txt)）                             |
| ビルド     | 本リポジトリの [scripts/build-ffmpeg-macos.sh](scripts/build-ffmpeg-macos.sh)                   |

VJDownloader が必要とする機能（`h264_videotoolbox` エンコード、ネイティブデコーダ、
`audiotoolbox` 出力、`rawvideo` パイプ、https 入力、ffprobe のタグ取得）はすべて
FFmpeg 内蔵で足りるため、外部ライブラリを一切リンクしていません。
GPLコンポーネント（libx264 / libx265 など）を含まないため、ライセンスは **LGPL-2.1-or-later** です。

ビルド時の configuration は次のとおりです。

```
--disable-autodetect --disable-debug --disable-doc --disable-ffplay
--enable-videotoolbox --enable-audiotoolbox --enable-securetransport
--enable-zlib --enable-bzlib --enable-iconv
--extra-cflags='-mmacosx-version-min=13.0' --extra-ldflags='-mmacosx-version-min=13.0'
--extra-libs=-liconv
```

同梱バイナリの正確な configuration は、次のコマンドでいつでも確認できます。

```bash
~/.vjdownloader/bin/ffmpeg -version
```

#### 対応するソースコードの入手先

LGPLの要求に従い、同梱バイナリに対応するソースコードの入手先を示します。
FFmpeg のソースへの改変は行っていません。

- FFmpeg 本体: <https://github.com/FFmpeg/FFmpeg/commit/1de77bb8987e2c7364302c91b9f13958e419124e>
- ビルド手順: [scripts/build-ffmpeg-macos.sh](scripts/build-ffmpeg-macos.sh)（上記コミットから同じバイナリを再現できます）
- LGPL / GPL のライセンス全文および各コンポーネントの条件:
  <https://github.com/FFmpeg/FFmpeg/blob/master/LICENSE.md>

上記が入手できない場合は、本リポジトリの Issues からご連絡ください。

#### 再配布する場合の注意

FFmpeg は独立した実行ファイルとして同梱し、サブプロセスとして起動しています。
VJDownloader 本体のコードと静的リンクしているわけではないため、本体コードは
MIT のままで問題ありません。ビルド済みバイナリを再配布する場合は、LGPL の要求に従い
ライセンス文（[licenses/LGPL-2.1.txt](licenses/LGPL-2.1.txt)）と上記のソース入手先を
同伴させてください。リリース用の `.app` には `Contents/Resources/` へ自動で同梱されます。

GPL版FFmpeg（libx264 / libx265 入り）へ差し替えると、配布物全体に
GPL-2.0-or-later の条件が及びます。差し替える場合はライセンス表示の見直しが必要です。

### Syphon Framework（BSD 3-Clause）

macOS向けの `.app` バンドルは `Contents/Frameworks/Syphon.framework` に
公式 Syphon Framework を同梱します（`.github/workflows/` でソースからビルドして配置）。

- 配布元: <https://github.com/Syphon/Syphon-Framework>

```
Syphon Framework License:

Copyright 2010 bangnoise (Tom Butterworth) & vade (Anton Marini).
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

* Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.

* Redistributions in binary form must reproduce the above copyright
notice, this list of conditions and the following disclaimer in the
documentation and/or other materials provided with the distribution.

* Neither the name of the Syphon Project nor the names of its contributors
may be used to endorse or promote products derived from this software
without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDERS BE LIABLE FOR ANY
DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND
ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

### Rust クレート

依存クレートは静的にリンクされます。大半は MIT または Apache-2.0 です。
`rusqlite` の `bundled` フィーチャー経由で組み込まれる SQLite はパブリックドメインです。

全依存クレートのライセンス一覧は、次のコマンドで生成できます。

```bash
cargo install cargo-license && cargo license
```

主要な依存クレートと配布元は [Cargo.toml](Cargo.toml) を参照してください。

## 実行時に取得されるもの（配布物には含みません）

以下は初回セットアップ時にユーザーの環境へダウンロードされるもので、
VJDownloader の配布物には含まれません。

| ソフトウェア                | ライセンス                                              | 配布元                                  |
|-----------------------------|---------------------------------------------------------|-----------------------------------------|
| yt-dlp                      | Unlicense（パブリックドメイン相当）                     | <https://github.com/yt-dlp/yt-dlp>      |
| Deno                        | MIT                                                     | <https://github.com/denoland/deno>      |
| FFmpeg / FFprobe（Windows） | GPL-2.0-or-later（`win64-gpl` / `winarm64-gpl` ビルド） | <https://github.com/BtbN/FFmpeg-Builds> |

Windows向けビルドはFFmpegを同梱しないため、Windowsの配布物にGPLの義務は生じません。

## 通信先

VJDownloader は次の外部サービスへ通信します。

| 用途                     | 通信先                                               |
|--------------------------|------------------------------------------------------|
| 外部ツールの取得         | GitHub Releases（yt-dlp / Deno / FFmpeg-Builds）     |
| 動画のダウンロード・再生 | ユーザーが指定したURLのホスト                        |
| AnimeThemes連携          | `animethemes.moe` の API / HTML および動画配信ホスト |
| 通信速度測定             | `speed.cloudflare.com`                               |

テレメトリや利用状況の送信は行いません。
