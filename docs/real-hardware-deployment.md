# Real Hardware Deployment

This guide describes the upstream `nixos-kexec` workflow for deploying NixOS to
real machines over SSH while delegating storage provisioning to `disk-nix`.

The target address is runtime state. DHCP can change it at any time, so pass the
current SSH target to the CLI and keep addresses out of NixOS modules, flake
outputs, and disk specs.

## Prerequisites

The machine that runs `nixos-kexec` needs:

- `ssh`, `scp`, and `timeout`
- a reviewed `disk-nix` install spec
- kexec kernel and initrd artifacts for the installer environment
- SSH root access to the current target address
- console, IPMI, or physical fallback access

The installer environment that is kexec'd on the target needs:

- SSH access for the same target address
- `kexec-tools`
- Nix with `nix-command` and flakes enabled
- enough firmware and network configuration to return over SSH
- `disk-nix`, or network access to run the configured `disk-nix` flake app
- storage tools required by the spec, such as ZFS, parted, dosfstools, or
  util-linux

## Build A Kexec Installer

`examples/kexec-installer.nix` builds a NixOS kexec tree with SSH,
NetworkManager, redistributable firmware, Intel Wi-Fi modules, `disk-nix`, ZFS,
partitioning tools, and hardware diagnostics.

```sh
nix build --impure --expr '
let
  nixpkgs = builtins.getFlake "github:NixOS/nixpkgs/nixos-unstable";
  disk-nix = builtins.getFlake "github:hermetic-foundation/disk-nix";
  installer = import ./examples/kexec-installer.nix {
    inherit disk-nix nixpkgs;
    authorizedKeys = [ (builtins.readFile /home/me/.ssh/id_ed25519.pub) ];
    system = "x86_64-linux";
  };
in
installer.config.system.build.kexecTree
'
```

The result contains:

- `result/bzImage`
- `result/initrd.gz`
- `result/kexec-boot`

When `--kexec-kernel` points at `result/bzImage`, `nixos-kexec` reads the
kernel command line from the sibling `kexec-boot` script. Pass
`--kexec-append` only for non-NixOS kexec artifacts or custom command lines.

## Build Or Generate The Disk Spec

Generate a disk-nix encrypted ZFS install spec with a stable by-id disk path:

```sh
disk-nix install template zfs-root \
  --disk /dev/disk/by-id/<target-disk> \
  --encrypt \
  --out ./disk-nix-install.json
```

Review the spec before using it. The template is destructive: it repartitions
the disk, formats EFI and swap partitions, creates a ZFS pool, and creates ZFS
datasets.

For hardware where the disk can enumerate differently after reboot, use
`/dev/disk/by-id/wwn-*`, `/dev/disk/by-id/nvme-*`, or another stable by-id path.
Avoid `/dev/sd*` names in install specs.

## Public Flake Flow

Use this when the installer can fetch and build the target host directly:

```sh
target="root@<current-target-address>"

nixos-kexec plan "$target" \
  --flake github:you/flake#host \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./result/bzImage \
  --kexec-initrd ./result/initrd.gz \
  --ssh-tty

nixos-kexec script "$target" \
  --flake github:you/flake#host \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./result/bzImage \
  --kexec-initrd ./result/initrd.gz \
  --ssh-tty \
  --script-out ./nixos-kexec-install.sh
```

Review the generated script and the disk-nix command plan before executing:

```sh
nixos-kexec run "$target" \
  --flake github:you/flake#host \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./result/bzImage \
  --kexec-initrd ./result/initrd.gz \
  --ssh-tty \
  --execute
```

`--ssh-tty` is required for encrypted ZFS specs that use
`keylocation=prompt`, because the remote ZFS command must be able to prompt for
the passphrase.

## Local Or Private Flake Flow

Use `--flake-source` when the flake path is local or private. `nixos-kexec`
uploads the flake after the target enters the installer environment and rewrites
the install handoff to the staged path.

```sh
target="root@<current-target-address>"

nixos-kexec run "$target" \
  --flake path:/home/me/flake#host \
  --flake-source /home/me/flake \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./result/bzImage \
  --kexec-initrd ./result/initrd.gz \
  --ssh-tty \
  --execute
```

The `--flake` value still needs the host fragment. The path before the fragment
is used to determine the staged installable inside the remote installer.

## Prebuilt System Closure Flow

Use `--system` when the installer should not build the host closure. This is the
preferred path for private flakes, slow target hardware, and live media with
limited channel or cache access.

```sh
system="$(
  nix build --no-link --print-out-paths \
    path:/home/me/flake#nixosConfigurations.host.config.system.build.toplevel
)"

target="root@<current-target-address>"

nixos-kexec run "$target" \
  --flake path:/home/me/flake#host \
  --system "$system" \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./result/bzImage \
  --kexec-initrd ./result/initrd.gz \
  --ssh-option BatchMode=yes \
  --ssh-option IdentitiesOnly=yes \
  --ssh-option IdentityFile=/home/me/.ssh/id_ed25519 \
  --ssh-tty \
  --execute
```

With `--system`, `nixos-kexec` asks `disk-nix` to mount the target, copies the
local closure into the target store with `nix copy --no-check-sigs`, and runs
`nixos-install --system ... --no-channel-copy`.

## Review And Safety

`run` refuses to mutate without `--execute`. Use `plan` and `script` first.

Before execution, confirm:

- the target address is the current machine you intend to overwrite
- the disk spec uses the correct stable by-id path
- the disk-nix plan marks destructive actions explicitly
- encrypted ZFS prompts will be visible through `--ssh-tty`
- you have console, IPMI, or physical recovery access

The workflow performs destructive storage changes only through the uploaded
`disk-nix` spec and its policy. Keep install specs next to operator runbooks or
deployment artifacts, not hidden inside host modules.

## Post-Install Verification

After the final reboot and any encryption prompt, verify the installed system:

```sh
ssh "$target" 'hostname'
ssh "$target" 'findmnt -no SOURCE,FSTYPE /'
ssh "$target" 'zpool status -x || true'
ssh "$target" 'zfs get -H -o name,property,value encryption,keystatus,compression zroot/root'
ssh "$target" 'swapon --show'
ssh "$target" 'systemctl --failed --no-pager --plain'
ssh "$target" 'nmcli -t -f DEVICE,TYPE,STATE device status || true'
```

Expected results depend on the host configuration, but a healthy encrypted ZFS
install should show the intended hostname, a ZFS root, an available encryption
key, healthy pools, configured swap, reachable SSH, and zero failed systemd
units.
