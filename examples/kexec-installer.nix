{
  authorizedKeys ? [ ],
  disk-nix,
  networkManagerProfiles ? { },
  nixpkgs,
  system ? builtins.currentSystem,
}:

let
  pkgs = import nixpkgs { inherit system; };
in
nixpkgs.lib.nixosSystem {
  inherit system;

  modules = [
    "${nixpkgs}/nixos/modules/installer/netboot/netboot-minimal.nix"
    (
      { pkgs, ... }:
      {
        system.stateVersion = "25.11";
        hardware.enableRedistributableFirmware = true;
        hardware.firmware = [ pkgs.linux-firmware ];

        networking.hostName = "nixos-kexec-installer";
        networking.firewall.enable = false;
        networking.networkmanager = {
          enable = true;
          wifi.backend = "iwd";
          ensureProfiles.profiles = networkManagerProfiles;
        };

        nix.settings.experimental-features = [
          "nix-command"
          "flakes"
        ];

        services.openssh = {
          enable = true;
          settings = {
            PasswordAuthentication = false;
            PermitRootLogin = "yes";
          };
        };
        users.users.root.openssh.authorizedKeys.keys = authorizedKeys;

        boot = {
          initrd.availableKernelModules = [
            "cfg80211"
            "e1000e"
            "mac80211"
            "iwlwifi"
            "iwldvm"
          ];
          initrd.kernelModules = [
            "e1000e"
            "iwlwifi"
            "iwldvm"
          ];
          kernelModules = [
            "e1000e"
            "iwlwifi"
            "iwldvm"
          ];
          kernelParams = [
            "console=ttyS0"
            "console=tty0"
            "panic=1"
          ];
          supportedFilesystems = [
            "vfat"
            "zfs"
          ];
          zfs.forceImportRoot = false;
        };

        environment.systemPackages = [
          disk-nix.packages.${system}.disk-nix
          pkgs.dosfstools
          pkgs.gitMinimal
          pkgs.gnutar
          pkgs.iw
          pkgs.kexec-tools
          pkgs.networkmanager
          pkgs.openssh
          pkgs.parted
          pkgs.pciutils
          pkgs.usbutils
          pkgs.util-linux
          pkgs.zfs
        ];
      }
    )
  ];
}
