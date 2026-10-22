# cicerone3270 Rules

このファイルは、`cicerone3270` リポジトリで動作する AI Agent 向けの動作ルールを定義する。

## 1. このリポジトリの位置づけ

- 3270 端末の画面を案内付きで操作する、汎用の TUI (Rust + Ratatui) を持つ。特定の環境 (nuage-cluster など) に依存させない
- 組み込みの辞書 (`guide/`) には MVS 3.8j・MVS/CE の一般的な内容だけを書く。特定の環境のデータセット・レコード定義・運用の注意は、その環境のリポジトリがサイトの辞書として持つ (例: nuage-cluster の `nix/hosts/mainframe/guide/`、nuage-usage-ledger の `guide/`)
- 試し用の MVS (`nix/mvsce-local.nix`) も MVS/CE の標準の構成から作り、特定の環境の構成を持ち込まない

## 2. 変更の確かめ方

- `cargo test` と `cargo clippy --all-targets` を通す。`cargo fmt` で整形する。Nix のファイルは `nix run nixpkgs#nixfmt -- flake.nix nix/*.nix` で整形する
- 画面の判定・説明を変えたら、実際の画面で確かめる。`--script` で画面を出さずに操作できる (README の「辞書を書く」)
- 確かめる相手は `nix run .#mvsce-local` で立ち上げる手元の MVS (127.0.0.1:3270、IBMUSER / SYS1) を使う。使い終わったら Ctrl+C で止める
- `--script` でログオンしたら、最後に必ず `logoff` を入れる。入れないと TSO のセッションが残り、次のログオンが拒否される (IKJ56425I)。残った場合は `logon ユーザー reconnect` で戻って LOGOFF する

## 3. 辞書を書くときの決まり

- 説明は である・する調 で書く。メインフレームを知らない人が読む前提で、用語は最初に意味を添える
- 画面の判定条件は、実際に採取した画面の文字から決める。推測で書かない
- メッセージ ID の個別の説明は、実際に見たものだけ書く。意味が確かでないものは接頭辞ごとの一般的な説明に任せる
- `keys` で打つ操作は、データを変える操作 (削除・編集の保存など) を入れない
- 組み込みの画面の `id` は、サイトの辞書が `extends` で参照する。変えるときは README に書き、利用側に知らせる

## 4. その他

- Git の操作 (commit・push・ブランチ作成) は、指示が無い限り行わない
