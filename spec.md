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
- 設定ファイルは`~/.vjdownloader/settings.properties`。
- 設定キー`download.dir`が存在し空でない場合、その値を保存先として使用する。
- 設定ファイルは`#`または`!`で始まる行をコメントとして無視する。
- 設定ファイルは`key=value`または`key:value`形式の行のみを読む。

## 初回セットアップ画面
- yt-dlpまたはDenoが未導入・実行不可の場合に初回セットアップ画面を表示する。
- 初回セットアップ画面は独立したウィンドウとして表示する。
- 初回セットアップ画面でyt-dlp/Denoの状態とバージョンを表示する。
- `自動セットアップ`でyt-dlp/Denoを取得し、完了後に状態を更新する。

## 設定画面
- `Cmd+,`でも設定画面を開ける。
- macOSのメニューバー（Appメニュー）から`設定...`を開ける。
- 設定画面は独立したウィンドウとして表示する。
- 出力先フォルダ、YouTube認証（ブラウザクッキー）の設定を編集できる。
- 出力先フォルダはボタンからmacOSのフォルダ選択UIで指定できる。
- クッキー利用時はブラウザ名が必須で、未入力の場合は保存できない。
- 保存時に出力先フォルダが存在しない場合は作成を試みる。
- yt-dlp/Denoのバージョンとステータスを表示し、`最新を取得`で再取得できる。

## クッキー設定
- 設定キー`cookies.from_browser.enabled`が`true`のときのみクッキー取得を有効化する。
- 設定キー`cookies.from_browser.browser`が空の場合はクッキー取得を無効扱いとする。
- 設定キー`cookies.from_browser.profile`が空でない場合は`browser:profile`形式を使用する。
- クッキー取得はyt-dlpの`--cookies-from-browser`オプションとして渡す。

## 内部パス
- アプリ用データは`~/.vjdownloader`配下を使用する。
- `~/.vjdownloader/bin`にツール用のバイナリを配置する。
- yt-dlpは`~/.vjdownloader/bin/yt-dlp`に保存する。
- ffmpegは`~/.vjdownloader/bin/ffmpeg`を参照する。
- ffprobeは`~/.vjdownloader/bin/ffprobe`を参照する。
- denoは`~/.vjdownloader/bin/deno`を参照する。

## ダウンロード開始
- ダウンロード開始はクリップボードの文字列をそのままURLとして利用する。
- クリップボードに文字列がない、または空の場合は何もしない。

## ダウンロード処理
- ダウンロードは別スレッドで実行する。
- 起動時にバックグラウンドでyt-dlp/denoの有無を確認し、未導入ならGitHubの最新リリースから取得する。
- yt-dlpをダウンロードした後、実行権限を付与する。
- ffmpeg/ffprobeは同梱バイナリから`~/.vjdownloader/bin`へコピーし、実行権限を付与する。
- denoが存在しない場合はGitHubの最新リリースから`deno-aarch64-apple-darwin.zip`をダウンロードし展開する。
- yt-dlpが実行可能でない場合はダウンロードを開始しない。
- 保存先フォルダが存在しない場合は作成する。
- 出力テンプレートは`%(title)s.%(ext)s`を使用する。
- yt-dlp実行時に`~/.vjdownloader/bin`をPATH先頭に追加する。
- yt-dlpのstdout/stderrは行単位で読み取り、ログと進捗に反映する。
- ダウンロード中にStopを押した場合は実行中のプロセスを終了してキャンセルする。

## ダウンロードオプション（優先モード）
- `--no-playlist`を指定する。
- `--extractor-args youtube:player_client=web`を指定する。
- `--extractor-args youtube:skip=translated_subs`を指定する。
- `--concurrent-fragments 4`を指定する。
- `-S vcodec:h264,res,acodec:m4a`を指定する。
- `--match-filter vcodec~='(?i)^(avc|h264)'`を指定する。
- `--merge-output-format mp4`と`--ffmpeg-location`を指定する。
- `--js-runtimes deno`を指定する。
- 優先モードが失敗した場合は互換モードで再試行する。

## ダウンロードオプション（互換モード）
- `--no-playlist`を指定する。
- `--extractor-args youtube:player_client=web`を指定する。
- `--extractor-args youtube:skip=translated_subs`を指定する。
- `--concurrent-fragments 4`を指定する。
- `-f bv*[height<=720]+ba/b[height<=720]`を指定する。
- `--recode-video mp4`を指定する。
- `--postprocessor-args VideoConvertor:-c:v h264_videotoolbox -b:v 5M -pix_fmt yuv420p`を指定する。
- `--ffmpeg-location`を指定する。
- `--js-runtimes deno`を指定する。

## AnimeThemes専用パイプライン
- URLに`animethemes.moe`を含む場合に専用パイプラインへ分岐する。
- ファイル名はURLパスを基にした`.mp4`（タイムスタンプ付き）を使用する。
- 直リンク取得（優先）: ページHTMLを`curl -sL -m 8 -A <UA>`で取得し`og:video`または`video src`から`https://.../*.webm`を抽出する（先頭約30KBで打ち切る）。
- 直リンクを取得できた場合は`curl -L -m 120 --fail -o - -A <UA> <直リンク>`の出力をffmpegへパイプする。
- 直リンク取得に失敗した場合は`yt-dlp --no-playlist --concurrent-fragments 4 -f "bv+ba/b" --ffmpeg-location <ffmpeg> -o - <ページURL>`の出力をffmpegへパイプする。
- ffmpegは`-loglevel error -analyzeduration 100M -probesize 100M -f webm -i pipe:0 -c:v h264_videotoolbox -b:v 5M -pix_fmt yuv420p -c:a aac -b:a 192k -ignore_unknown -movflags +faststart -f mp4 -y <出力パス>`で実行する。

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
- yt-dlp出力中の`%`表記から進捗率を抽出する。進捗率が100%でも変換中には切り替えない。

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
- メインボタンの表示は待機時`Download`、ダウンロード中は`Stop`。
- サブタイトルに`リストをドラッグしてVDMXへドロップ`を表示する。
- ダウンロード中もメインボタンは有効で、クリックするとキャンセルする。

## テーマとフォント
- ダークテーマを適用する。
- ベース背景色は`rgb(12, 18, 32)`を使用する。
- 主要なアクセントカラーは`rgb(16, 190, 255)`を使用する。
- ボタンやパネルは角丸を使用する。
- フォントはSF系フォントを優先し、無い場合はAvenir系を使用する。
- 日本語フォントはHiragino Sans等のシステムフォントから順に使用する。

## エラーハンドリング
- 保存先作成失敗時はダウンロードを中断し、エラーを表示する。
- yt-dlp起動失敗時はダウンロード失敗として扱う。
- ファイル削除失敗時はステータスに`削除に失敗しました: <error>`を表示する。
- ドラッグ開始失敗時はステータスにエラーを表示する。
