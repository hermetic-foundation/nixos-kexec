{
  authorizedKeys ? [ ],
  disk-nix,
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
        networking.hostName = "nixos-kexec-installer";
        networking.firewall.enable = false;

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
          kernelParams = [
            "console=ttyS0"
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
          pkgs.gnutar
          pkgs.kexec-tools
          pkgs.openssh
          pkgs.parted
          pkgs.util-linux
          pkgs.zfs
        ];
      }
    )
  ];
}
