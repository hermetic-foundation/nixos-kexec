{
  description = "NixOS deployment through kexec with disk-nix storage management";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          package = pkgs.rustPlatform.buildRustPackage {
            pname = "nixos-kexec";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.installShellFiles ];
            postInstall = ''
              installShellCompletion --cmd nixos-kexec \
                --bash <($out/bin/nixos-kexec completions bash) \
                --zsh <($out/bin/nixos-kexec completions zsh) \
                --fish <($out/bin/nixos-kexec completions fish)
            '';
            meta = {
              description = "NixOS deployment through kexec with disk-nix storage management";
              homepage = "https://github.com/hermetic-foundation/nixos-kexec";
              license = pkgs.lib.licenses.mit;
              mainProgram = "nixos-kexec";
            };
          };
        in
        {
          default = package;
          nixos-kexec = package;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/nixos-kexec";
          meta.description = "NixOS deployment through kexec with disk-nix storage management";
        };
      });

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = self.packages.${system}.default;
          formatting = pkgs.runCommand "nixos-kexec-formatting-check" { } ''
            ${pkgs.rustfmt}/bin/cargo-fmt --manifest-path ${self}/Cargo.toml --check
            touch "$out"
          '';
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.clippy
              pkgs.rustc
              pkgs.rustfmt
              pkgs.jujutsu
              pkgs.openssh
            ];
          };
        }
      );
    };
}
