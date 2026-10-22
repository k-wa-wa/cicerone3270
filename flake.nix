{
  description = "3270 端末の画面を案内付きで操作する TUI (MVS 3.8j・MVS/CE の TSO・ISPF 向け)";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # 辞書 (guide/*.toml) はバイナリに埋め込むため、パッケージは実行ファイル 1 つである
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "ci3270";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./guide
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          meta.mainProgram = "ci3270";
        };

        # 試し・検証用の MVS (MVS/CE on Hercules) を手元で動かす。nix run .#mvsce-local
        mvsce-local = pkgs.callPackage ./nix/mvsce-local.nix { };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
          ];
        };
      });
    };
}
