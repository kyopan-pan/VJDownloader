# VJDownloader 仕様

## 概要
- クリップボードのURLから動画をダウンロードし、ローカルのMP4を管理・送出するmacOS向けツール。
- UIはダーク基調で、ダウンロード進捗と保存済みリストを表示する。
- アプリ名は「VJ Downloader (Rust)」で表示される。

## ウィンドウ
- 初期サイズは幅420px・高さ720px。
- 最小サイズは幅360px・高さ640px。

## 保存先と設定
- 既定の保存先は`~/Movies/YtDlpDownloads`。
- 設定ファイルは`~/.ytdownloader/settings.properties`。
- 設定キー`download.dir`が存在し空でない場合、その値を保存先として使用する。
- 設定ファイルは`#`または`!`で始まる行をコメントとして無視する。
- 設定ファイルは`key=value`または`key:value`形式の行のみを読む。

## クッキー設定
- 設定キー`cookies.from_browser.enabled`が`true`のときのみクッキー取得を有効化する。
- 設定キー`cookies.from_browser.browser`が空の場合はクッキー取得を無効扱いとする。
- 設定キー`cookies.from_browser.profile`が空でない場合は`browser:profile`形式を使用する。
- クッキー取得はyt-dlpの`--cookies-from-browser`オプションとして渡す。

## 内部パス
- アプリ用データは`~/.ytdownloader`配下を使用する。
- `~/.ytdownloader/bin`にツール用のバイナリを配置する。
- yt-dlpは`~/.ytdownloader/bin/yt-dlp`に保存する。
- ffmpegは`~/.ytdownloader/bin/ffmpeg`を参照する。
- denoは`~/.ytdownloader/bin/deno`を参照する。

## ダウンロード開始
- ダウンロード開始はクリップボードURLを利用する。
- クリップボードにURLがない場合はステータスにエラーを表示して終了する。
- クリップボード内に`URL=`で始まる行がある場合、そのURLを優先的に抽出する。
- クリップボード内にwebloc形式(XML)がある場合、そのURLを抽出する。
- 文字列中の`http://`または`https://`で始まるトークンをURLとして検出する。
- URLは`http`または`https`のみ有効とする。

## ダウンロード処理
- ダウンロードは別スレッドで実行する。
- yt-dlpが存在しない場合はGitHubの最新リリースから`yt-dlp_macos`をダウンロードする。
- yt-dlpをダウンロードした後、実行権限を付与する。
- 保存先フォルダが存在しない場合は作成する。
- 出力テンプレートは`%(title)s.%(ext)s`を使用する。
- yt-dlp実行時に`~/.ytdownloader/bin`をPATH先頭に追加する。
- yt-dlpのstdout/stderrは行単位で読み取り、ログと進捗に反映する。
- yt-dlpの出力から保存パスを検出した場合、ファイル一覧を更新し`Saved: <filename>`をログに追加する。

## ダウンロードオプション（優先モード）
- `--print after_move:filepath`で保存パスを取得する。
- `--no-playlist`を指定する。
- `--extractor-args youtube:player_client=web`を指定する。
- `--extractor-args youtube:skip=translated_subs`を指定する。
- `--concurrent-fragments 4`を指定する。
- `-S vcodec:h264,res,acodec:m4a`を指定する。
- `--match-filter vcodec~='(?i)^(avc|h264)'`を指定する。
- ffmpegが存在する場合は`--merge-output-format mp4`と`--ffmpeg-location`を指定する。
- denoが存在する場合は`--js-runtimes deno`を指定する。
- 優先モードが失敗した場合は互換モードで再試行する。

## ダウンロードオプション（互換モード）
- `--print after_move:filepath`で保存パスを取得する。
- `--no-playlist`を指定する。
- `--extractor-args youtube:player_client=web`を指定する。
- `--extractor-args youtube:skip=translated_subs`を指定する。
- `--concurrent-fragments 4`を指定する。
- ffmpegが存在する場合は`-f bv*[height<=720]+ba/b[height<=720]`を指定する。
- ffmpegが存在する場合は`--recode-video mp4`を指定する。
- ffmpegが存在する場合は`--postprocessor-args VideoConvertor:-c:v h264_videotoolbox -b:v 5M -pix_fmt yuv420p`を指定する。
- ffmpegが存在しない場合は`-f b[height<=720]`を指定する。
- denoが存在する場合は`--js-runtimes deno`を指定する。

## 進捗表示
- 進捗パネルは常に表示され、待機中は半透明表示となる。
- 進捗メッセージの初期値は`待機中...`。
- ダウンロード開始直後は`動画読み込み中...`を表示する。
- 進捗率が取得できる場合は`ダウンロード中... xx.x%`を表示する。
- 変換や結合が始まった場合は`変換中...`を表示する。
- 完了時は`ダウンロード完了!`を表示する。
- 完了後1.2秒で進捗表示を非表示(待機状態)に戻す。
- 進捗率が不明な場合はインジケータをアニメーション表示する。

## 進捗の判定
- yt-dlp出力に`[merger]`や`[ffmpeg]`などの語が出現した場合は変換フェーズと判定する。
- yt-dlp出力中の`%`表記から進捗率を抽出する。

## ファイル一覧
- 保存先フォルダ内の`.mp4`のみを表示する。
- 一覧は最終更新日時の降順で並べる。
- 一覧の表示高は360pxで固定する。
- リストが空の場合は`まだダウンロードがありません。`を表示する。
- 行右端の`✕`ボタンで削除できる。
- ファイル名は左寄せで表示する。
- ファイル名の上下パディングは等間隔に揃える。
- ファイル名が長い場合は末尾を`...`で省略する。

## Drag & Drop
- リスト項目のドラッグでmacOSネイティブのファイルドラッグを開始する。
- Finderと同様に、ドラッグ中はファイルアイコンが表示される。
- ドロップ先へはファイル参照が渡る。
- ファイル名を含むホバーでハイライトされる範囲全体がドラッグ対象。
- ホバー時はマウスカーソルをクリックマークに変更する。
- クリック等の動作は不要（ドラッグ専用）。
- ドラッグ開始時はファイルパスを正規化し、失敗時はステータスにエラーを表示する。
- ドラッグ用アイコンはmacOSのGenericDocumentIconを使用する。

## ステータスログ
- ステータスは最大200行まで保持する。
- `Status`セクションは折りたたみ式で、初期状態は閉じている。
- ログは下方向にスクロール可能で、最大表示高は120px。
- `ログをクリア`ボタンでログを削除し、`ログをクリアしました。`を追加する。

## UIテキスト
- メインボタンの表示は`Download`。
- サブタイトルに`リストをドラッグしてVDMXへドロップ`を表示する。
- ダウンロード中はメインボタンが無効化され、ホバー時に`ダウンロード中です`を表示する。

## テーマとフォント
- ダークテーマを適用する。
- ベース背景色は`rgb(12, 18, 32)`を使用する。
- 主要なアクセントカラーは`rgb(16, 190, 255)`を使用する。
- ボタンやパネルは角丸を使用する。
- フォントはSF系フォントを優先し、無い場合はAvenir系を使用する。
- 日本語フォントはHiragino Sans等のシステムフォントから順に使用する。

## エラーハンドリング
- クリップボードアクセス失敗時はステータスにエラーを表示する。
- 保存先作成失敗時はダウンロードを中断し、エラーを表示する。
- yt-dlp起動失敗時はダウンロード失敗として扱う。
- ファイル削除失敗時はステータスに`削除に失敗しました: <error>`を表示する。
- ドラッグ開始失敗時はステータスにエラーを表示する。
