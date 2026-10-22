# MVS/CE (MVS 3.8j Community Edition) のリリース物。https://github.com/MVS-sysgen/sysgen
# DASD イメージ (ディスク)・Hercules の構成・起動用 rc ファイル・停止用スクリプトを store に置く。
# DASD は MVS が書き換え続けるため、ここにある版は「初期イメージ」として扱い、mvsce-local が初回だけコピーする。
{ runCommand, fetchurl }:

let
  version = "3.0.0";
  src = fetchurl {
    url = "https://github.com/MVS-sysgen/sysgen/releases/download/v${version}/MVSCE.release.v${version}.tar";
    hash = "sha256-ld7EXZOWtN64m0Gq98HwEQ4u/xZawKnEY/y/xD+aGkw=";
  };
in
# MVP/ は sysgen 時にパッケージを組み立てるためのもので、稼働には不要なので除く
runCommand "mvsce-${version}" { } ''
  tar xf ${src} --exclude='MVSCE/MVP'
  mkdir -p $out
  cp -r MVSCE/DASD MVSCE/SCRIPTS MVSCE/conf MVSCE/herclogo.txt $out/
''
