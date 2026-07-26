{
  nixpkgs,
  package,
  pkgs,
  system,
}:

let
  testPublicKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKmfJ/u7zZYX/eGpGBCm4o7eN3FJEQ8sjOTr4SZg+JTL nixos-kexec-test";
  testPrivateKey = pkgs.writeText "nixos-kexec-test-key" ''
    -----BEGIN OPENSSH PRIVATE KEY-----
    b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
    QyNTUxOQAAACCpnyf7u82WF/3hqRgQpuKO3jdxSREPLIzk6+EmYPiUywAAAJjnwLc258C3
    NgAAAAtzc2gtZWQyNTUxOQAAACCpnyf7u82WF/3hqRgQpuKO3jdxSREPLIzk6+EmYPiUyw
    AAAEByD9+aG//u8cyjSXDbfel0+ETTWaI0y6FdQ0zbubeyQ6mfJ/u7zZYX/eGpGBCm4o7e
    N3FJEQ8sjOTr4SZg+JTLAAAAEG5peG9zLWtleGVjLXRlc3QBAgMEBQ==
    -----END OPENSSH PRIVATE KEY-----
  '';
  fakeDiskNix = pkgs.writeShellApplication {
    name = "disk-nix";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      set -euo pipefail
      mkdir -p /var/log/nixos-kexec
      printf '%s\n' "$*" >> /var/log/nixos-kexec/disk-nix.log

      case "$1" in
        apply)
          test "$2" = "--spec"
          test -f "$3"
          test "$4" = "--probe-current"
          test "$5" = "--execute"
          ;;
        install)
          test "$2" = "nixos"
          target=""
          while [ "$#" -gt 0 ]; do
            case "$1" in
              --target)
                target="$2"
                shift 2
                ;;
              *)
                shift
                ;;
            esac
          done
          test -n "$target"
          mkdir -p "$target"
          printf 'installed\n' > "$target/nixos-kexec-install-marker"
          ;;
        *)
          echo "unexpected disk-nix command: $*" >&2
          exit 1
          ;;
      esac
    '';
  };
  installer = nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [
      "${nixpkgs}/nixos/modules/installer/netboot/netboot-minimal.nix"
      (
        { pkgs, ... }:
        {
          system.stateVersion = "25.11";
          networking.hostName = "nixos-kexec-installer";
          networking.firewall.enable = false;
          services.openssh = {
            enable = true;
            settings = {
              PermitRootLogin = "yes";
              PasswordAuthentication = false;
            };
          };
          users.users.root.openssh.authorizedKeys.keys = [ testPublicKey ];
          boot.kernelParams = [
            "console=ttyS0"
            "panic=1"
          ];
          boot.zfs.forceImportRoot = false;
          environment.systemPackages = [
            fakeDiskNix
            pkgs.kexec-tools
            pkgs.openssh
          ];
          systemd.services.nixos-kexec-stage-marker = {
            wantedBy = [ "multi-user.target" ];
            serviceConfig.Type = "oneshot";
            script = ''
              printf 'installer\n' > /run/nixos-kexec-stage
            '';
          };
        }
      )
    ];
  };
  kexecTree = installer.config.system.build.kexecTree;
  kexecAppend = "init=${installer.config.system.build.toplevel}/init ${toString installer.config.boot.kernelParams}";
  testPath = pkgs.lib.makeBinPath [
    pkgs.coreutils
    pkgs.openssh
  ];
in
pkgs.testers.runNixOSTest {
  name = "nixos-kexec-kexec-vm";
  globalTimeout = 900;

  nodes.machine =
    { pkgs, ... }:
    {
      system.stateVersion = "25.11";
      networking.firewall.enable = false;
      services.openssh = {
        enable = true;
        settings = {
          PermitRootLogin = "yes";
          PasswordAuthentication = false;
        };
      };
      users.users.root.openssh.authorizedKeys.keys = [ testPublicKey ];
      boot.kernel.sysctl."kernel.kexec_load_disabled" = false;
      boot.zfs.forceImportRoot = false;
      boot.kernelParams = [ "console=ttyS0" ];
      environment.systemPackages = [
        pkgs.kexec-tools
        pkgs.openssh
      ];
      virtualisation.memorySize = 1536;
    };

  testScript = ''
    import os
    import socket
    import stat
    import subprocess
    import tempfile
    from pathlib import Path

    def free_port():
        sock = socket.socket()
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
        sock.close()
        return port

    machine.start(allow_reboot=True)
    port = free_port()
    machine.forward_port(port, 22)
    machine.wait_for_unit("sshd.service")
    machine.wait_for_open_port(22)
    machine.succeed("test \"$(cat /proc/sys/kernel/kexec_load_disabled)\" = 0")

    work = Path(tempfile.mkdtemp(prefix="nixos-kexec-vm-"))
    key = work / "id_ed25519"
    key.write_text(${builtins.toJSON (builtins.readFile testPrivateKey)})
    key.chmod(stat.S_IRUSR | stat.S_IWUSR)
    spec = work / "disk-nix-install.json"
    spec.write_text('{"version":1,"apply":{"mode":"vm-test"}}\n')

    ssh_options = [
        "--ssh-option", f"Port={port}",
        "--ssh-option", f"IdentityFile={key}",
        "--ssh-option", "IdentitiesOnly=yes",
        "--ssh-option", "StrictHostKeyChecking=no",
        "--ssh-option", "UserKnownHostsFile=/dev/null",
        "--ssh-option", "LogLevel=ERROR",
        "--ssh-option", "ConnectTimeout=5",
    ]
    env = os.environ.copy()
    env["PATH"] = "${testPath}:" + env.get("PATH", "")
    smoke_ssh = [
        "ssh",
        "-p", str(port),
        "-i", str(key),
        "-o", "IdentitiesOnly=yes",
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "LogLevel=ERROR",
        "root@127.0.0.1",
        "true",
    ]
    subprocess.run(smoke_ssh, check=True, env=env, timeout=60)

    command = [
        "${package}/bin/nixos-kexec",
        "run",
        "root@127.0.0.1",
        "--flake", "path:/tmp/fake-flake#host",
        "--disk-spec", str(spec),
        "--kexec-kernel", "${kexecTree}/bzImage",
        "--kexec-initrd", "${kexecTree}/initrd.gz",
        "--kexec-append", ${builtins.toJSON kexecAppend},
        "--disk-nix-command", "disk-nix",
        "--target-root", "/mnt",
        "--no-final-reboot",
        "--execute",
    ] + ssh_options
    subprocess.run(command, check=True, env=env, timeout=720)

    ssh = [
        "ssh",
        "-p", str(port),
        "-i", str(key),
        "-o", "IdentitiesOnly=yes",
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "LogLevel=ERROR",
        "root@127.0.0.1",
    ]
    subprocess.run(ssh + ["test -f /run/nixos-kexec-stage"], check=True, env=env, timeout=60)
    disk_nix_log = subprocess.check_output(ssh + ["cat /var/log/nixos-kexec/disk-nix.log"], env=env, text=True, timeout=60)
    assert "apply --spec /tmp/nixos-kexec/disk-nix-install.json --probe-current --execute" in disk_nix_log
    assert "install nixos --spec /tmp/nixos-kexec/disk-nix-install.json --flake path:/tmp/fake-flake#host --target /mnt --execute" in disk_nix_log
    subprocess.run(ssh + ["test -f /mnt/nixos-kexec-install-marker"], check=True, env=env, timeout=60)
    machine.connected = False
  '';
}
