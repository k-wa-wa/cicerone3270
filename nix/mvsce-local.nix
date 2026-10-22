{
  lib,
  callPackage,
  writeShellApplication,
  hercules,
  coreutils,
  curl,
  gnugrep,
  gnused,
}:

# 試し・検証用の MVS (MVS/CE on Hercules) を手元で動かす。コマンドを実行している間だけ動き、
# Ctrl+C (またはターミナルを閉じる) で正規の手順で止まる。
#
#   nix run .#mvsce-local                     起動して IPL の完了を待つ (Ctrl+C で停止)
#   nix run .#mvsce-local -- --reset          DASD を初期状態に戻してから起動する
#   nix run .#mvsce-local -- cmd '/D A,L'     動いている MVS のコンソールにコマンドを送る (別の端末から)
#   nix run .#mvsce-local -- submit x.jcl     動いている MVS のカードリーダに JCL を流す (別の端末から)
#
# MVS/CE の標準の構成 (conf/local.cnf) を使い、手元の機械を守るために次の点だけを変える。
#   - 3270 (TN3270) は 127.0.0.1 でだけ待ち受ける
#   - MVS からホストのシェルコマンドを実行する機能 (SHCMDOPT) を無効にする
#   - NJE38 (別の MVS との通信装置) の構成を読まない
#   - コマンドの投入口として HTTP を足す。待ち受けアドレスを指定できないため、起動のたびに作るパスワードで認証を付ける
# DASD は状態ディレクトリ (既定 ~/.local/state/mvsce-local) に置き、次回の起動でもそのまま使う。

let
  mvsce = callPackage ./mvsce.nix { };
in
writeShellApplication {
  name = "mvsce-local";
  runtimeInputs = [
    hercules
    coreutils
    curl
    gnugrep
    gnused
  ];
  text = ''
    state="''${MVSCE_LOCAL_DIR:-''${XDG_STATE_HOME:-$HOME/.local/state}/mvsce-local}"

    usage() {
      cat <<'EOF'
    使い方: mvsce-local [--reset]        試し・検証用の MVS を起動する (Ctrl+C で停止)
            mvsce-local cmd コマンド     動いている MVS のコンソールにコマンドを送る (例: cmd '/D A,L')
            mvsce-local submit JCL       動いている MVS のカードリーダに JCL を流す

    接続先:   TN3270 127.0.0.1:3270 (ci3270 127.0.0.1:3270 など)
    ユーザー: IBMUSER / SYS1 (MVS/CE の初期値)
    状態:     $MVSCE_LOCAL_DIR (既定 ~/.local/state/mvsce-local)。DASD・コンソールのログ・プリンタの出力を置く
    EOF
    }

    # 動いている MVS の HTTP サーバーへコマンドを送る (パスワードは起動時に状態ディレクトリへ書く)
    send_cmd() {
      local auth
      auth=$(cat "$state/.http-auth" 2>/dev/null) || {
        echo "mvsce-local が動いていない" >&2
        return 1
      }
      curl -sf -o /dev/null --max-time 10 -u "$auth" \
        http://127.0.0.1:8038/cgi-bin/tasks/cmd --data-urlencode "cmd=$*"
    }

    case "''${1:-}" in
      -h | --help)
        usage
        exit 0
        ;;
      cmd)
        shift
        send_cmd "$@"
        echo "送った。結果は $state/console.log に出る"
        exit 0
        ;;
      submit)
        shift
        cat -- "$@" > /dev/tcp/127.0.0.1/3505
        echo "投入した。結果は $state/printers/prt00e.txt に出る"
        exit 0
        ;;
      --reset | "") ;;
      *)
        usage >&2
        exit 2
        ;;
    esac

    for port in 3270 3505 3506 8038; do
      if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
        echo "127.0.0.1:$port を別のプロセスが使っている。mvsce-local がもう動いていないか確かめる" >&2
        exit 1
      fi
    done

    mkdir -p "$state/DASD" "$state/printers" "$state/punchcards"
    cd "$state"
    if [ "''${1:-}" = "--reset" ]; then
      echo "DASD を初期状態に戻す"
      rm -f DASD/*
    fi
    # 無いものだけを初期イメージから置く (2 回目からは前回の DASD を使う)
    for disk in ${mvsce}/DASD/*; do
      if [ ! -e "DASD/$(basename "$disk")" ]; then
        install -m 0600 "$disk" DASD/
      fi
    done
    ln -sfn ${mvsce}/SCRIPTS SCRIPTS
    ln -sfn ${mvsce}/herclogo.txt herclogo.txt
    rm -f mvslog.txt

    umask 077
    password=$(LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 24 || true)
    printf 'mvslocal:%s' "$password" > .http-auth
    {
      sed -e 's/^CNSLPORT .*/CNSLPORT  127.0.0.1:3270/' \
        -e 's/^SHCMDOPT .*/SHCMDOPT  DISABLE NODIAG8/' \
        -e '/^INCLUDE /d' \
        ${mvsce}/conf/local.cnf
      printf 'HTTP      PORT 8038 AUTH mvslocal %s\nHTTP      START\n' "$password"
    } > hercules.cnf
    grep -q '^CNSLPORT  127.0.0.1:3270' hercules.cnf
    grep -q '^SHCMDOPT  DISABLE' hercules.cnf
    if grep -q '^INCLUDE ' hercules.cnf; then exit 1; fi

    # 端末の Ctrl+C が Hercules に直接届かないよう、別のプロセスグループで動かす (set -m)。
    # 止めるときは下の stop が正規の手順 (SHUTDOWN.RC) を踏む
    set -m
    hercules --NoUI -f hercules.cnf -r ${mvsce}/conf/mvsce.rc < /dev/null > console.log 2>&1 &
    pid=$!
    set +m

    # ターミナルを閉じたとき (HUP) も同じ手順で止める。止めている間は重ねて届くシグナルを無視し、
    # 表示に失敗しても (閉じたターミナルへの出力) 止める処理は続ける
    stop() {
      trap - EXIT
      trap "" INT TERM HUP
      set +e
      if kill -0 "$pid" 2>/dev/null; then
        echo
        echo "MVS を止めている (JES2 停止 → QUIESCE → Hercules 終了。30 秒ほどかかる)"
        send_cmd script SCRIPTS/SHUTDOWN.RC || kill "$pid"
        for _ in $(seq 1 180); do
          kill -0 "$pid" 2>/dev/null || break
          sleep 1
        done
        if kill -0 "$pid" 2>/dev/null; then
          echo "3 分で止まらなかったため Hercules を終了させる"
          kill "$pid"
        fi
      fi
      rm -f "$state/.http-auth"
      echo "止まった"
    }
    trap stop INT TERM HUP EXIT

    # IPL の完了 (TSO がログオンを受け付け始めた) を待つ。
    # 出力が 20 秒止まったら IPL 中の問い合わせへの応答を送り直す (自動応答がまれに取りこぼされるため)
    echo "IPL している (状態: ''${state}、コンソール: ''${state}/console.log)"
    last_count=-1
    stalled_since=$SECONDS
    deadline=$((SECONDS + 600))
    until grep -q "IKT007I TCAS ACCEPTING LOGONS" console.log 2>/dev/null; do
      if ! kill -0 "$pid" 2>/dev/null; then
        echo "Hercules が終了した。console.log を確かめる" >&2
        tail -20 console.log >&2
        exit 1
      fi
      if ((SECONDS > deadline)); then
        echo "600 秒以内に IPL が完了しなかった" >&2
        exit 1
      fi
      count=$(wc -l < console.log)
      if [ "$count" != "$last_count" ]; then
        last_count=$count
        stalled_since=$SECONDS
      elif ((SECONDS - stalled_since >= 20)); then
        if grep -E 'IEA101A|HASP426' console.log | tail -1 | grep -q 'HASP426'; then
          send_cmd '/r 0,noreq' || true
        else
          send_cmd '/' || true
        fi
        stalled_since=$SECONDS
      fi
      sleep 2
    done

    cat <<EOF
    MVS が起動した。Ctrl+C で止める。

      TN3270:     127.0.0.1:3270   (例: ci3270 127.0.0.1:3270)
      ユーザー:   IBMUSER / SYS1
      コンソール: tail -f $state/console.log
      コマンド:   mvsce-local cmd '/D A,L'
    EOF
    wait "$pid" || true
  '';
  meta.description = "試し・検証用の MVS (MVS/CE on Hercules) を手元で動かす";
  meta.platforms = lib.platforms.unix;
}
