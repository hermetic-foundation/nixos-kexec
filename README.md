# nixos-kexec

`nixos-kexec` deploys NixOS by using SSH to kexec a controlled installer
environment, then running `disk-nix` for storage provisioning and NixOS install
handoff.

The first transport is SSH. The command model is centered on kexec, so local or
agent-based frontends can be added later without changing the core phases.

## Status

This repository currently provides an initial CLI that renders and can execute a
reviewable SSH orchestration script. It can also render or build the upstream
kexec installer tree used by that script.

The local machine running `nixos-kexec` must have `ssh`, `scp`, and `timeout`
available.

The installer environment must boot with:

- SSH access for the same target address
- enough network firmware and tooling for the target hardware
- Nix with `nix-command` and flakes available
- `kexec-tools`
- network access to the NixOS flake, unless `--flake-source` stages it

If the installer image already includes a compatible `disk-nix` executable, pass
`--disk-nix-command disk-nix` to avoid fetching the flake app at install time.
Without that option, the installer also needs network access to the configured
`disk-nix` flake app.

For local or private flake directories, pass `--flake-source /path/to/flake`.
`nixos-kexec` uploads that directory after kexec and rewrites the install
handoff to use the staged path inside the installer. The `--flake` value must
still include the target host fragment, such as `path:/home/me/flake#host`.

For encrypted storage specs that prompt for a passphrase, pass `--ssh-tty` so
mutating remote commands can allocate a TTY.

For hosts that must decrypt first-boot secrets with their SSH host identity,
pass `--host-key /path/to/ssh_host_ed25519_key`. `nixos-kexec` copies that
private key over SSH after `disk-nix` has mounted the target and installs it as
`/etc/ssh/ssh_host_ed25519_key` before `nixos-install` runs. If
`<host-key>.pub` exists, it is installed next to the private key; otherwise pass
`--host-key-public /path/to/ssh_host_ed25519_key.pub`.

Host keys passed this way are deployment secrets. Keep them outside flake
source, outside the Nix store, and in an encrypted backup or password manager if
more than one trusted operator machine needs to perform installs.

`nixos-kexec` is an install orchestrator, not a replacement spelling for
`nixos-rebuild --target-host`. A deployment must enter the kexec installer,
apply the uploaded `disk-nix` spec, and then install into the mounted target.
The plan output names the install strategy so operators can see where the
target system closure will come from before anything mutates:

- installer evaluates the flake and builds or downloads the target system
- `--flake-source` uploads a local flake, then the installer builds or downloads
  the target system
- `--system` copies a prebuilt closure into the mounted target store, then runs
  `nixos-install --system`

For private flakes or hosts that should not build in the installer, build the
system closure locally and pass `--system /nix/store/...-nixos-system-host`.
`nixos-kexec` then asks `disk-nix` to mount the target, copies the closure into
the mounted target store, and runs `nixos-install --system --no-channel-copy`.
That avoids reading channel state from the installer media after the target
system closure has already been staged.

When `--kexec-kernel` points inside a NixOS kexec tree, `nixos-kexec` reads the
kernel command line from the sibling `kexec-boot` script. For non-NixOS kexec
artifacts, pass `--kexec-append` explicitly.

## Usage

See [examples](./examples/) for end-to-end command flows and sample
`disk-nix` install specs. The examples include `kexec-installer.nix`, which can
build a kexec installer tree with SSH, NetworkManager, redistributable firmware,
explicit Intel Wi-Fi module loading, `disk-nix`, ZFS, partitioning tools, and
hardware diagnostics available. It accepts optional NetworkManager profiles for
targets that must rejoin Wi-Fi after kexec.

Use [Real hardware deployment](./docs/real-hardware-deployment.md) for the full
operator workflow. That guide covers dynamic target addresses, local/private
flake staging, locally built system closures, disk-nix install specs, encrypted
ZFS prompts, and post-install verification.

Build the upstream kexec installer tree:

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

Render a plan:

```sh
nixos-kexec plan root@192.0.2.10 \
  --flake github:you/flake#host \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./bzImage \
  --kexec-initrd ./initrd
```

Render a reviewable script:

```sh
nixos-kexec script root@192.0.2.10 \
  --flake github:you/flake#host \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./bzImage \
  --kexec-initrd ./initrd \
  --script-out ./nixos-kexec-install.sh
```

Run the orchestration only after reviewing the script:

```sh
nixos-kexec run root@192.0.2.10 \
  --flake path:/home/me/flake#host \
  --flake-source /home/me/flake \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./bzImage \
  --kexec-initrd ./initrd \
  --ssh-tty \
  --execute
```

Install a locally built system closure:

```sh
system="$(
  nix build --no-link --print-out-paths \
    path:/home/me/flake#nixosConfigurations.host.config.system.build.toplevel
)"

nixos-kexec run root@192.0.2.10 \
  --flake path:/home/me/flake#host \
  --system "$system" \
  --host-key /home/me/host-keys/host/ssh_host_ed25519_key \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./bzImage \
  --kexec-initrd ./initrd.gz \
  --ssh-option BatchMode=yes \
  --ssh-option IdentitiesOnly=yes \
  --ssh-option IdentityFile=/home/me/.ssh/id_ed25519 \
  --ssh-tty \
  --execute
```

For tests or staged handoffs where another process handles the final restart,
add `--no-final-reboot`.

The generated workflow:

1. Checks remote root access and required kexec tooling.
2. Uploads the installer kernel and initrd.
3. Loads and enters the kexec installer.
4. Waits for SSH to return.
5. Optionally uploads a local flake source.
6. Uploads the `disk-nix` install spec.
7. Runs `disk-nix apply --execute`.
8. Runs `disk-nix install nixos --execute`, or splits the handoff when needed:
   `disk-nix install mount`, optional host-key provisioning, optional closure
   copy, then `nixos-install`.
9. Reboots into the installed system.

## Safety

`run` refuses to execute unless `--execute` is present. `plan` and `script`
are the intended review path before mutating a host.

Keep console, IPMI, or physical access available when testing. Kexec replaces
the running kernel immediately, and storage provisioning can be destructive
when the `disk-nix` spec formats disks.

## Testing

Run the full local check suite:

```sh
nix flake check
```

The `e2eCli` check runs the packaged binary against fixture kernel, initrd, and
disk-nix spec files. It validates JSON plan output, reviewable script rendering,
the executable script mode, shell completions, and refusal paths that must not
touch a remote host.

On `x86_64-linux`, the `kexecVm` check boots a NixOS VM, reaches it over SSH,
runs the packaged `nixos-kexec run`, kexecs into a NixOS netboot installer
image, reconnects over SSH, and verifies the post-kexec disk-nix apply/install
handoff.
