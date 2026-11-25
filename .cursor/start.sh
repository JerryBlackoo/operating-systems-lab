#!/usr/bin/env bash
# Per-boot reconciliation for the ctr container-runtime lab.
# Idempotent: safe to run repeatedly; tolerates an already-configured VM.
set -euo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

# The Cloud Agent VM root is itself an overlay filesystem, so native
# `mount -t overlay` cannot use an upperdir/workdir on `/`. Provide a
# non-overlay (tmpfs) scratch area for the runtime's overlay upper/work dirs.
SCRATCH=/var/lib/ctr
sudo mkdir -p "$SCRATCH"
if ! mountpoint -q "$SCRATCH"; then
  sudo mount -t tmpfs -o mode=1777 tmpfs "$SCRATCH"
fi

# Ensure the busybox rootfs is present even after a fresh checkout (cheap rebuild).
if [ ! -x busybox-rootfs/bin/busybox ] && [ -x /usr/bin/busybox ]; then
  bash .cursor/make-rootfs.sh busybox-rootfs || true
fi

# Ensure cgroup v2 cpu + memory controllers are delegated to child cgroups so the
# runtime can create per-container cgroups with --memory / --cpus limits.
for c in cpu memory; do
  if ! grep -qw "$c" /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null; then
    echo "+$c" | sudo tee /sys/fs/cgroup/cgroup.subtree_control >/dev/null 2>&1 || true
  fi
done

echo "start.sh: overlay scratch=$SCRATCH (tmpfs); cgroup subtree_control=[$(cat /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null)]"
