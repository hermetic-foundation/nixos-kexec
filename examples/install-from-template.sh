#!/usr/bin/env bash
set -euo pipefail

: "${TARGET:?Set TARGET, for example root@192.0.2.10}"
: "${FLAKE:?Set FLAKE, for example github:you/flake#host}"
: "${DISK:?Set DISK to a stable path, for example /dev/disk/by-id/wwn-...}"
: "${KEXEC_KERNEL:?Set KEXEC_KERNEL to the installer bzImage path}"
: "${KEXEC_INITRD:?Set KEXEC_INITRD to the installer initrd path}"

WORKDIR="${WORKDIR:-./build/nixos-kexec-example}"
SPEC="${SPEC:-$WORKDIR/disk-nix-install.json}"
SCRIPT="${SCRIPT:-$WORKDIR/nixos-kexec-install.sh}"
FLAKE_SOURCE_ARGS=()
SSH_TTY_ARGS=()

if [ -n "${FLAKE_SOURCE:-}" ]; then
  FLAKE_SOURCE_ARGS=(--flake-source "$FLAKE_SOURCE")
fi

if [ "${SSH_TTY:-0}" = 1 ]; then
  SSH_TTY_ARGS=(--ssh-tty)
fi

mkdir -p "$WORKDIR"

disk-nix install template zfs-root \
  --disk "$DISK" \
  --encrypt \
  --out "$SPEC"

nixos-kexec plan "$TARGET" \
  --flake "$FLAKE" \
  "${FLAKE_SOURCE_ARGS[@]}" \
  --disk-spec "$SPEC" \
  --kexec-kernel "$KEXEC_KERNEL" \
  --kexec-initrd "$KEXEC_INITRD" \
  "${SSH_TTY_ARGS[@]}"

nixos-kexec script "$TARGET" \
  --flake "$FLAKE" \
  "${FLAKE_SOURCE_ARGS[@]}" \
  --disk-spec "$SPEC" \
  --kexec-kernel "$KEXEC_KERNEL" \
  --kexec-initrd "$KEXEC_INITRD" \
  "${SSH_TTY_ARGS[@]}" \
  --script-out "$SCRIPT"

printf 'review %s, then run:\n' "$SCRIPT"
printf '  %s\n' "$SCRIPT"
