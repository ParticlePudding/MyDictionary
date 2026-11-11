# fastdict

Rust + windows-rs(Win32 標準コントロール)+ SQLite(静的リンク)の個人用辞書。
起動が速く、入力中に絞り込めることを重視しています。

## 使い方
- `Ctrl+Alt+D`(設定で変更可)で呼び出し。Esc / 閉じるボタンで隠す。終了はトレイアイコン右クリック。
- **全体検索**: 見出し語・意味・タグを横断。スペース区切りで AND、大文字小文字無視、部分一致。
- **詳細**: チェックすると「見出し語 / 意味 / タグ」ごとの検索欄が出る(全体検索とAND)。タグは複数指定でAND。
- **辞書**: 右上のドロップダウンで「すべて」または特定の辞書を検索。登録先もこの選択(「すべて」なら最初の書き込み可能な辞書)。
- **登録**: 見出し語・タグ(スペース/カンマ/読点区切り)・意味を入力して Ctrl+Enter。
- **編集**: 行をダブルクリック、または「選択を編集」→ 保存は Ctrl+Enter / 「更新」。
- **削除**: 行を選択(Ctrl/Shift で複数)→ Delete キー / 「選択を削除」。確認ダイアログあり。

## 設定ファイル `fastdict.ini`
場所は exe と同じフォルダ(あればそちら。ポータブル運用)、なければ `%AppData%\fastdict\`。
初回起動時に自動作成されます。トレイ右クリック →「設定ファイルを開く」「設定を再読み込み」。

```ini
[general]
hotkey = Ctrl+Alt+D      ; none で無効。修飾キー(Ctrl/Alt/Shift/Win)必須
close_to_tray = true
timing_log = false       ; true で timing.log に起動時間などを記録

[dictionary.main]        ; 先頭の辞書が既定の登録先
name = メイン
path = dictionary.db     ; 相対パスは設定ファイル基準。%APPDATA% 等も可

[dictionary.ref]
name = 参照用
path = D:\dict\ref.db
readonly = true          ; 参照のみ(登録・編集・削除不可)
```

## 起動時間の計測
- `fastdict.exe --bench [件数]` … 指定件数(既定10万)のテストDBを作り、読み込み・並べ替え・検索の時間を表示します(結果は `%AppData%\fastdict\bench.txt` にも保存)。実データには触りません。
- `timing_log = true`(または `--timing` 付きで起動)… 実際の起動で「プロセス開始 → ウィンドウ表示 → 読み込み完了 → 各検索」の時間を `timing.log`(設定ファイルと同じ場所)に記録します。

## ビルド
```
cargo run --release
cargo test
```
