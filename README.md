# cicerone3270

3270 端末の画面を、案内を見ながら操作するための TUI。コマンドは `ci3270`。

cicerone (チチェローネ) は、遺跡や美術館を案内する案内人のこと。
古いメインフレームの画面を一つ一つ説明しながら歩く、という意味で付けた。
コマンド名は x3270 の一族 (x3270 / c3270 / s3270) に合わせて短くしている。

MVS 3.8j (MVS/CE など、Hercules で動くメインフレーム) の TSO・ISPF を初めて触る人が
「いま何の画面で、画面の各部分は何で、次に何ができるか」を分かりながら操作できるようにする。

- 左: 3270 の画面 (24 行 80 桁)。入力欄は下線、色は 3270 の属性どおり
- 右: いまの画面の説明と「できること」。クリックすると、そのキー操作を代わりに打つ
- マウスを画面の文字に乗せると、ツールチップで説明が出る
  - メッセージ ID (IKJ56425I など)、画面の部品 (Command ===> など)、用語の意味
  - データセット一覧の列 (VOLUME・LRECL など) の意味
  - ブラウズ画面では、レコード定義を辞書に書いたデータセットなら、何桁目が何の項目か。16 進表示ならパック 10 進数などの値も読む
  - その位置の欄の種類 (入力欄・表示専用・非表示など)
- 下: 状態 (入力できる / 応答待ち)・カーソル位置と、PF キーなどのボタン

組み込みの辞書は MVS 3.8j・MVS/CE の一般的な内容だけを持つ。
自分の環境に固有のこと (データセットの意味、レコード定義、運用の注意) は、サイトの辞書として外から足す。

## 使い方

```bash
nix run github:k-wa-wa/cicerone3270 -- ホスト:ポート    # ci3270 を動かす。既定の接続先は localhost:3270
```

マウスの移動を受け取るため、iTerm2 などの端末で使う。tmux の中では `set -g mouse on` が要る。

| キー | 3270 での意味 |
| :-- | :-- |
| Enter | Enter (入力をホストへ送る) |
| Tab / Shift+Tab | 次 / 前の入力欄へ |
| Home | 最初の入力欄 (コマンド欄) へ |
| F1〜F12 / Shift+F1〜F12 | PF1〜PF12 / PF13〜PF24 |
| Ctrl+C | CLEAR |
| Ctrl+P | PA1 (TSO の中断) |
| Ctrl+E | 欄の残りを消す (Erase EOF) |
| Ctrl+R | キーボードのロック解除 (Reset) |
| Insert | 挿入モードの切り替え |
| Ctrl+L | 辞書を読み直す |
| Ctrl+Q | 終了 (ログオン中は 2 回押す) |
| マウスのクリック | 3270 の画面ならカーソル移動、ボタンなら実行 |

LOGOFF せずに終了すると TSO のセッションが残り、次のログオンが拒否される (IKJ56425I)。
その画面の「できること」から RECONNECT で前回のセッションに戻れる。

### 試し用の MVS (mvsce-local)

接続先が無くても試せるように、MVS/CE を手元で動かすコマンドを同梱している。
コマンドを実行している間だけ動き、Ctrl+C (またはターミナルを閉じる) で正規の手順 (JES2 停止 → QUIESCE) で止まる。

```bash
nix run github:k-wa-wa/cicerone3270#mvsce-local          # 起動して IPL の完了を待つ (初回は約 1 分)
nix run github:k-wa-wa/cicerone3270 -- 127.0.0.1:3270    # 別の端末から接続する (IBMUSER / SYS1)

nix run github:k-wa-wa/cicerone3270#mvsce-local -- cmd '/D A,L'     # コンソールにコマンドを送る
nix run github:k-wa-wa/cicerone3270#mvsce-local -- submit job.jcl   # カードリーダに JCL を流す
nix run github:k-wa-wa/cicerone3270#mvsce-local -- --reset          # DASD を初期状態に戻して起動する
```

| 項目 | 内容 |
| :-- | :-- |
| 構成 | MVS/CE v3.0.0 の標準の構成 (`conf/local.cnf`) に、手元の機械を守る変更だけを加える。3270 は 127.0.0.1 でだけ待ち受ける。MVS からホストのシェルコマンドを実行する機能 (SHCMDOPT) は無効にし、NJE38 (別の MVS との通信装置) は読まない。コマンドの投入口の HTTP は待ち受けアドレスを指定できないため、起動のたびに作るパスワードで認証を付ける |
| 状態 | `~/.local/state/mvsce-local` (環境変数 `MVSCE_LOCAL_DIR` で変えられる)。DASD を次回の起動でもそのまま使う。コンソールのログ (`console.log`)・プリンタの出力 (`printers/`) もここに出る |

## 辞書

辞書は TOML で、次の 3 種類を書ける。

| 種類 | 内容 |
| :-- | :-- |
| `[[screen]]` | 画面の判定条件 (`match`: 画面の文字に当てる正規表現)、説明、できること (`[[screen.action]]` のキー操作)、領域ごとの注釈 (`[[screen.region]]`)、ブラウズ画面の指定 (`[screen.browse]`) |
| `[[term]]` | 画面のどこに出ても説明する語 (`pattern`: 1 行に当てる正規表現) |
| `[[layout]]` | データセットのレコード定義 (`dataset` の正規表現と、桁ごとの項目。`kind` は char / zoned / packed) |

組み込みの辞書は `guide/screens.toml` と `guide/terms.toml` で、ビルド時にバイナリに埋め込む。

### サイトの辞書を足す

```bash
ci3270 --guide ./my-guide ホスト:ポート          # 何度でも指定でき、先のものほど優先する
CI3270_GUIDE_PATH=/a:/b ci3270 ホスト:ポート     # 環境変数でも足せる (無いディレクトリは無視する)
```

サイトの辞書は組み込みの辞書より先に読み、先に読んだものを優先する
(同じ語に当たる `term` は先のものだけを出し、判定は先に書いた `screen` から試す)。
`extends` を書いた `screen` は、組み込みの画面に「できること」・注釈・説明を足す。

```toml
# データセット一覧の入口に「できること」を足す
[[screen]]
id = "my-dslist-entry"
extends = "dslist-entry"
description = "業務のデータセットは MYAPP.* にある。"   # 説明の末尾に足す

[[screen.action]]
label = "業務のデータセット (MYAPP) を一覧"
keys = "{home}{tab}{eraseeof}MYAPP{enter}"
```

`keys` の文字はそのまま入力し、`{enter}` `{pf3}` `{pa1}` `{clear}` `{tab}` `{home}` `{eraseeof}` はそのキーを押す。
組み込みの画面の `id` は `guide/screens.toml` を参照する。

使っている例: [nuage-cluster](https://github.com/k-wa-wa/nuage-cluster) の mainframe ホストは、
基盤の辞書と業務アプリ ([nuage-usage-ledger](https://github.com/k-wa-wa/nuage-usage-ledger) の勘定マスタのレコード定義など) の辞書を足して起動している。

### 辞書を書く

辞書は Ctrl+L で読み直せる。組み込みの辞書そのものを書くときは、埋め込み版の代わりにファイルを読ませる。

```bash
cargo run -- --no-builtin --guide guide 127.0.0.1:3270
```

辞書に無い画面を足すときは、`--script` で実際の画面を採取して判定条件を決める。

```bash
# ログオン画面を出して、画面の文字・入力欄・できること・23 行 3 桁の説明を出す
cargo run -- --script "wait; enter; waitfor TSO Logon; dump; explain 23 3" 127.0.0.1:3270
```

`--script` は画面を出さずに手順 (`;` 区切り) を流す。ログオンした場合は最後に `logoff` を入れる
(入れないとセッションが残る)。

## 仕組み

```
ci3270 ──TN3270 (TCP 3270)──▶ Hercules の 3270 端末装置 ──▶ VTAM / TSO
   │
   ├─ telnet.rs   telnet の交渉 (端末型 IBM-3278-2-E、BINARY、EOR) とレコードの切り出し
   ├─ screen.rs   3270 データストリームの解釈 (Write・オーダー・属性) と、送るレコード (AID・変更された欄) の組み立て
   ├─ guide.rs    辞書による画面の判定と、マウス位置の説明
   └─ app.rs      TUI (Ratatui)
```

3270 の通信は外部の端末エミュレータに頼らず、このプログラムが直接話す。
このため入力欄の属性 (保護・数字・非表示・変更済み) を正確に知っており、ツールチップに出せる。

## 開発

```bash
nix develop        # cargo / rustc / clippy / rustfmt
cargo test
cargo clippy --all-targets
nix build          # パッケージ (テストも走る)
```

通信の中身を見るときは `CI3270_TRACE=1` で、送受信したレコードを 16 進で標準エラーに出す。

## 実装で分かったこと

| 事柄 | 内容 |
| :-- | :-- |
| 端末型は -E 付きで名乗る | `IBM-3278-2` (拡張なし) を名乗ると、Hercules は WSF (端末への問い合わせ) を命令エラーにする。TSO はログオン時に問い合わせるため、エラー回復 (IKT00405I) で画面を消してしまう。`IBM-3278-2-E` を名乗り、Read Partition Query に Query Reply で答える |
| ESC 付きの書き込み | VTAM のログオン画面は `F5 27 F5 C3 ...` のように、データの先頭に ESC (X'27') とコマンドが重なって届く。x3270 と同じく WCC = X'27' として読み、続く 2 バイトは文字として扱う (直後の SBA で上書きされるので画面は崩れない) |
| セッションが残る | 切断すると TSO は「再接続待ち」(IKT108I) でセッションを残す。`LOGON ユーザー RECONNECT` で戻れ、PA2 で画面を描き直せる |
| IPL 直後のログオン | IPL 直後の最初のログオンは、READY が出るまで 20 秒以上かかることがある |
| ブラウズの 16 進表示 | 1 画面に 40 バイト。既定の区切り (1〜40 / 41〜80) をまたぐ項目は、コマンド欄に桁数を打って PF11 で途中から表示する (ツールチップがその桁数を示す) |
| Hercules の待ち受け | 3270 (`CNSLPORT`) は `127.0.0.1:3270` の形で待ち受けアドレスを指定できるが、HTTP サーバーは指定できない |

## 今後

| 項目 | 内容 |
| :-- | :-- |
| AI の説明 | 辞書に無い画面・語を、画面の文字を LLM に送って説明させる |
| 辞書を増やす | ISPF のエディタ、SD (RevOut) のジョブ出力、KICKS の画面 |
| 画面の大きさ | モデル 2 (24 行 80 桁) だけに対応している |
