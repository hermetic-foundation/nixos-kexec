# nixos-kexec

`nixos-kexec` deploys NixOS by using SSH to kexec a controlled installer
environment, then running `disk-nix` for storage provisioning and NixOS install
handoff.

The first transport is SSH. The command model is centered on kexec, so local or
agent-based frontends can be added later without changing the core phases.

## Status

This repository currently provides an initial CLI that renders and can execute a
reviewable SSH orchestration script. It expects you to supply the kexec kernel
and initrd artifacts for the installer environment.

The local machine running `nixos-kexec` must have `ssh`, `scp`, and `timeout`
available.

The installer environment must boot with:

- SSH access for the same target address
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

## Usage

See [examples](./examples/) for end-to-end command flows and sample
`disk-nix` install specs. The examples include `kexec-installer.nix`, which can
build a kexec installer tree with SSH, `disk-nix`, ZFS, partitioning tools, and
tar available.

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
8. Runs `disk-nix install nixos --execute`.
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
