# Examples

These examples show the `nixos-kexec` deployment layer. The detailed storage
schema belongs to `disk-nix`; use these files as starting points for the
installer handoff and replace all placeholder paths before executing anything.

## Files

- `install-from-template.sh` generates a `disk-nix` install spec with the
  upstream template command, renders a `nixos-kexec` plan, and writes a
  reviewable deployment script.
- `install-with-preinstalled-disk-nix.sh` uses an installer image that already
  contains a compatible `disk-nix` executable.
- `kexec-installer.nix` builds a NixOS kexec installer tree with SSH,
  `disk-nix`, ZFS, partitioning tools, and tar available.
- `specs/simple-root.json` is a small non-destructive lifecycle-style spec for
  an existing root filesystem.
- `specs/zfs-encrypted-root.by-id.json` is an install-shape spec for encrypted
  ZFS root storage using a stable `/dev/disk/by-id/...` disk path.

## Review Flow

Build a kexec installer tree with your SSH public key:

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

Run the plan first:

```sh
nixos-kexec plan root@192.0.2.10 \
  --flake github:you/flake#host \
  --disk-spec ./examples/specs/zfs-encrypted-root.by-id.json \
  --kexec-kernel ./bzImage \
  --kexec-initrd ./initrd
```

Render a script for review:

```sh
nixos-kexec script root@192.0.2.10 \
  --flake github:you/flake#host \
  --disk-spec ./examples/specs/zfs-encrypted-root.by-id.json \
  --kexec-kernel ./bzImage \
  --kexec-initrd ./initrd \
  --script-out ./nixos-kexec-install.sh
```

Execute only after reviewing the script, confirming the target host, and
confirming the disk identifiers:

```sh
nixos-kexec run root@192.0.2.10 \
  --flake path:/home/me/flake#host \
  --flake-source /home/me/flake \
  --disk-spec ./examples/specs/zfs-encrypted-root.by-id.json \
  --kexec-kernel ./result/bzImage \
  --kexec-initrd ./result/initrd.gz \
  --ssh-tty \
  --execute
```
