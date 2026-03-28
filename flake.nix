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

    # Standalone kernel module package
    packages = forAllSystems (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in {
        ua-apollo = pkgs.linuxPackages.callPackage ./nix/ua-apollo-module.nix {};
      }
    );
  };
}
