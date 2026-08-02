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

## Bootstrap Host Identity

Some NixOS configurations decrypt secrets during activation with the target
host SSH key, for example through agenix age recipients based on
`/etc/ssh/ssh_host_ed25519_key`. On a first install that key may not exist yet.

Use `--host-key` when the installed system needs a stable host identity before
`nixos-install` runs:

```sh
ssh-keygen -t ed25519 \
  -f ~/.ssh/host-keys/host/ssh_host_ed25519_key \
  -C 'root@host bootstrap host key' \
  -N ''

# Add ~/.ssh/host-keys/host/ssh_host_ed25519_key.pub to your age recipients,
# then rekey secrets before deploying.
```

During deployment, pass the private key path:

```sh
nixos-kexec run "$target" \
  --flake github:you/flake#host \
  --host-key ~/.ssh/host-keys/host/ssh_host_ed25519_key \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./result/bzImage \
  --kexec-initrd ./result/initrd.gz \
  --ssh-tty \
  --execute
```

`nixos-kexec` copies the key over SSH after `disk-nix install mount`, installs
it as `/etc/ssh/ssh_host_ed25519_key` under the mounted target with mode `0600`,
installs the public key if available, removes the temporary remote copy, and
then runs `nixos-install`.

This is host identity provisioning. The private key is not part of the flake,
the installer image, or the Nix store. The deployment is authorized by whoever
holds that private host key, so store it in an encrypted password manager or
backup if future installs should not depend on a single operator machine.

If you do not want any deployer to hold the final host key, generate the key in
the live environment, copy only the public key back to your flake, rekey
secrets, and deploy with that private key still on the target. That avoids
off-target private-key custody but adds an interactive preflight step.

## Build A Kexec Installer

`nixos-kexec installer` builds a NixOS kexec tree with SSH, NetworkManager,
redistributable firmware, Intel Wi-Fi modules, `disk-nix`, ZFS, partitioning
tools, and hardware diagnostics.

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

The result contains:

- `result/bzImage`
- `result/initrd.gz`
- `result/kexec-boot`

When `--kexec-kernel` points at `result/bzImage`, `nixos-kexec` reads the
kernel command line from the sibling `kexec-boot` script. Pass
`--kexec-append` only for non-NixOS kexec artifacts or custom command lines.

`installer plan` prints the exact Nix build command and expression without
running it. `installer build` refuses to run unless `--execute` is present.
Authorized keys are runtime inputs to the installer artifact; they are not tied
to the target host SSH key, which may not exist before first install.

For Wi-Fi-only targets, pass a NetworkManager profiles JSON file:

```json
{
  "home-wifi": {
    "connection": {
      "id": "home-wifi",
      "type": "wifi",
      "autoconnect": true
    },
    "wifi": {
      "mode": "infrastructure",
      "ssid": "home-wifi"
    },
    "wifi-security": {
      "key-mgmt": "wpa-psk",
      "psk": "change-me",
      "psk-flags": 0
    },
    "ipv4": {
      "method": "auto"
    },
    "ipv6": {
      "method": "auto"
    }
  }
}
```

This embeds the profile into the installer tree. For small lab networks that may
be acceptable; for sensitive networks, build the installer artifact on trusted
storage and handle the profile as an operational secret.

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

The plan should report this install strategy:

```text
install strategy: installer evaluates the flake and builds or downloads the target system closure
```

In this mode `nixos-kexec` does not copy the NixOS system closure from the
operator machine. The installer must be able to fetch the flake and build or
substitute the target system.

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

The plan should report this install strategy:

```text
install strategy: nixos-kexec uploads local flake source; installer builds or downloads the target system closure
```

Do not combine `--flake-source` with `--system`. Those are different install
strategies: one asks the installer to build from source, and the other copies a
prebuilt system closure after `disk-nix` mounts the target.

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

The plan should report this install strategy:

```text
install strategy: nixos-kexec copies a prebuilt system closure into the mounted target store
```

This is the only mode where `nixos-kexec` copies the full target system closure.
If that closure contains a large personal desktop or development environment,
the copy can still be large; it happens after kexec and after storage has been
mounted by `disk-nix`, not as a direct `nixos-rebuild --target-host` switch into
the currently running system.

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
