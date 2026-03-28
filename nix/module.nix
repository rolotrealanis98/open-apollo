{ config, lib, pkgs, ... }:

let
  cfg = config.hardware.ua-apollo;
  ua-apollo-module = config.boot.kernelPackages.callPackage ./ua-apollo-module.nix {};
in
{
  options.hardware.ua-apollo = {
    enable = lib.mkEnableOption "Universal Audio Apollo Thunderbolt driver";
  };

  config = lib.mkIf cfg.enable {
    # Load the kernel module
    boot.extraModulePackages = [ ua-apollo-module ];
    boot.kernelModules = [ "ua_apollo" ];

    # IOMMU passthrough (required for most systems)
    boot.kernelParams = [ "iommu=pt" ];

    # Thunderbolt device manager
    services.hardware.bolt.enable = true;

    # PipeWire audio
    services.pipewire = {
      enable = true;
      alsa.enable = true;
      pulse.enable = true;
      wireplumber.enable = true;
    };
    security.rtkit.enable = true;

    # Required packages
    environment.systemPackages = with pkgs; [
      python3
      python3Packages.pyusb
      alsa-utils
    ];
  };
}
