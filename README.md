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

The installer environment must boot with:

- SSH access for the same target address
- Nix with `nix-command` and flakes available
- `kexec-tools`
- network access to the NixOS flake and `disk-nix`

## Usage

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
  --flake github:you/flake#host \
  --disk-spec ./disk-nix-install.json \
  --kexec-kernel ./bzImage \
  --kexec-initrd ./initrd \
  --execute
```

The generated workflow:

1. Checks remote root access and required kexec tooling.
2. Uploads the installer kernel and initrd.
3. Loads and enters the kexec installer.
4. Waits for SSH to return.
5. Uploads the `disk-nix` install spec.
6. Runs `disk-nix apply --execute`.
7. Runs `disk-nix install nixos --execute`.
8. Reboots into the installed system.

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
