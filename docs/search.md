# VJDownloader mp4高速検索エンジン

Rust + SQLite + notifyで、ローカルGUIアプリ向けのmp4インデックス検索を提供します。
検索時にフルスキャンせず、事前インデックス + 差分更新で低レイテンシ検索を行います。

## 何ができるか
- 複数ルートフォルダ配下の`.mp4`を事前インデックス
- 日本語を含むファイル名の部分一致検索
- メタデータ条件検索（`root` / `parent_dir` / `size` / `modified_time` / `limit` / `sort`）
- `notify`監視で追加・削除・リネームを差分反映
- GUI設定画面から検索対象フォルダの追加・削除、全再インデックス

## 起動手順
```bash
cargo run
```

## テスト
```bash
cargo test search_index::tests -- --test-threads=1
```

## 設計概要
- DB: SQLite（`~/.ytdownloader/search_index.sqlite3`）
- 書き込み: 単一ライタースレッド（キュー経由）
- 読み取り: 検索ワーカーでクエリ実行
- 初回/再構築: `walkdir`でフルスキャン
- 差分更新: `notify` + デバウンス（700ms）

### SQLiteを使う理由
- ローカル埋め込みで配布が容易（単一ファイル）
- トランザクションと索引により高速かつ安全
- WALモードで読み取りと更新の並行性を確保しやすい

## 日本語検索ポリシー
- `file_name_norm`に **NFKC + lower** を適用
- クエリ側にも同じ正規化を適用
- `%` / `_` / `\\`はLIKEエスケープして誤マッチを防止

制限:
- 意味的同義語検索（例: 表記ゆれ辞書）やかな漢字変換は未対応
- OS依存のパス表現差（シンボリックリンク経由など）は完全吸収していない

## 監視イベントの注意点
- OS監視イベントは順序入れ替わり・取りこぼしが起こり得ます
- 本実装はデバウンスとイベント統合で吸収し、監視エラー時は有効ルートを再スキャンします
- ずれた場合は設定画面の`全体を再インデックス`で復旧できます

## 大規模フォルダ運用の注意
- 初回スキャン時間はファイル数に比例して増加します
- ルートを絞るほどDBサイズと更新負荷を抑えられます
- 不要ディレクトリは検索対象ルートに含めない運用を推奨

## 将来拡張
- 動画メタ情報（長さ・解像度）の追加カラム
- タグ/お気に入りなどのユーザー属性
- ランキング強化（前方一致・最近利用・ルート重み）

## 実装主要ファイル
- `/Users/kyopan/Documents/VJSoft/VJDownloader/src/search_index.rs`
- `/Users/kyopan/Documents/VJSoft/VJDownloader/src/settings_ui.rs`
- `/Users/kyopan/Documents/VJSoft/VJDownloader/src/settings.rs`
- `/Users/kyopan/Documents/VJSoft/VJDownloader/src/app.rs`
