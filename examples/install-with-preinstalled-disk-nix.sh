#!/usr/bin/env bash
set -euo pipefail

: "${TARGET:?Set TARGET, for example root@192.0.2.10}"
: "${FLAKE:?Set FLAKE, for example github:you/flake#host}"
: "${DISK_SPEC:?Set DISK_SPEC to a disk-nix install spec path}"
: "${KEXEC_KERNEL:?Set KEXEC_KERNEL to the installer bzImage path}"
: "${KEXEC_INITRD:?Set KEXEC_INITRD to the installer initrd path}"

DISK_NIX_COMMAND="${DISK_NIX_COMMAND:-disk-nix}"
SCRIPT="${SCRIPT:-./nixos-kexec-install.sh}"

nixos-kexec plan "$TARGET" \
  --flake "$FLAKE" \
  --disk-spec "$DISK_SPEC" \
  --kexec-kernel "$KEXEC_KERNEL" \
  --kexec-initrd "$KEXEC_INITRD" \
  --disk-nix-command "$DISK_NIX_COMMAND"

nixos-kexec script "$TARGET" \
  --flake "$FLAKE" \
  --disk-spec "$DISK_SPEC" \
  --kexec-kernel "$KEXEC_KERNEL" \
  --kexec-initrd "$KEXEC_INITRD" \
  --disk-nix-command "$DISK_NIX_COMMAND" \
  --script-out "$SCRIPT"

printf 'review %s, then run:\n' "$SCRIPT"
printf '  %s\n' "$SCRIPT"

