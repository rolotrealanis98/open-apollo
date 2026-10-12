{
  description = "Open Apollo — Linux driver for Universal Audio Apollo interfaces";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
  let
    supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
    forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
  in
  {
    # NixOS module — add to your imports
    nixosModules.default = import ./nix/module.nix;

    packages = forAllSystems (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in {
        # Standalone kernel module package
        ua-apollo = pkgs.linuxPackages.callPackage ./nix/ua-apollo-module.nix {};

        # Native mixer console (console-rs/). UA's art is not included: it is
        # loaded at run time from the user's own UAD Console install.
        uaconsole = pkgs.callPackage ./nix/uaconsole.nix {};
      }
    );

    apps = forAllSystems (system: {
      uaconsole = {
        type = "app";
        program = nixpkgs.lib.getExe self.packages.${system}.uaconsole;
        meta.description = "Native mixer console for open-apollo";
      };
    });

    # `nix flake check` builds the console and runs its unit tests (no GPU needed).
    checks = forAllSystems (system: {
      uaconsole = self.packages.${system}.uaconsole;
    });

    # `nix develop` (or `nix develop .#uaconsole`): Rust toolchain plus the libraries
    # and fonts uaconsole loads at run time, for `cargo run` inside console-rs/.
    devShells = forAllSystems (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        uaconsole = self.packages.${system}.uaconsole;
        shell = pkgs.mkShell {
          inputsFrom = [ uaconsole ];
          packages = with pkgs; [
            clippy
            rustfmt
            rust-analyzer
            python3 # mixer-engine/ua_mixer_daemon.py
          ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath uaconsole.passthru.runtimeLibs;
          FONTCONFIG_FILE = "${uaconsole.passthru.fontsConf}";
        };
      in {
        default = shell;
        uaconsole = shell;
      }
    );
  };
}
