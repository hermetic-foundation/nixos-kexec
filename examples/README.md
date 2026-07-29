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
  NetworkManager, redistributable firmware, explicit Intel Wi-Fi module loading,
  `disk-nix`, ZFS, partitioning tools, and hardware diagnostics available.
- `specs/simple-root.json` is a small non-destructive lifecycle-style spec for
  an existing root filesystem.
- `specs/zfs-encrypted-root.by-id.json` is an install-shape spec for encrypted
  ZFS root storage using a stable `/dev/disk/by-id/...` disk path.

## Review Flow

The SSH target is always runtime state. Do not bake DHCP addresses into a host
flake or disk spec. Discover the current address from DHCP, mDNS, DNS, ARP, or a
console, then pass it as `TARGET=root@<current-address>`.

Build a kexec installer tree with your SSH public key:

```sh
nixos-kexec installer plan \
  --authorized-key-file ~/.ssh/id_ed25519.pub \
  --network-manager-profiles-json ./network-profiles.json \
  --out-link ./result

nixos-kexec installer build \
  --authorized-key-file ~/.ssh/id_ed25519.pub \
  --network-manager-profiles-json ./network-profiles.json \
  --out-link ./result \
  --execute
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

For private flakes, build the host locally and pass the resulting system path:

```sh
system="$(
  nix build --no-link --print-out-paths \
    path:/home/me/flake#nixosConfigurations.host.config.system.build.toplevel
)"

nixos-kexec run root@192.0.2.10 \
  --flake path:/home/me/flake#host \
  --system "$system" \
  --disk-spec ./examples/specs/zfs-encrypted-root.by-id.json \
  --kexec-kernel ./result/bzImage \
  --kexec-initrd ./result/initrd.gz \
  --ssh-option BatchMode=yes \
  --ssh-option IdentitiesOnly=yes \
  --ssh-option IdentityFile=/home/me/.ssh/id_ed25519 \
  --ssh-tty \
  --execute
```
