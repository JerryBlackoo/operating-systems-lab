#!/usr/bin/env bash
# Build (idempotently) the busybox rootfs used by tests/run_tests.sh.
# The acceptance tests default to `--rootfs ./busybox-rootfs`, so it is created
# at the repository root. Requires the `busybox-static` package (see install.sh).
set -euo pipefail

ROOT="${1:-busybox-rootfs}"

if [ ! -x /usr/bin/busybox ]; then
  echo "make-rootfs: /usr/bin/busybox missing; run .cursor/install.sh first" >&2
  exit 1
fi

mkdir -p "$ROOT"/bin "$ROOT"/proc "$ROOT"/sys "$ROOT"/dev "$ROOT"/tmp "$ROOT"/etc
cp -f /usr/bin/busybox "$ROOT"/bin/busybox

# Create an applet symlink for every busybox command (sh, echo, ls, ps, hostname, ...).
(
  cd "$ROOT"/bin
  while IFS= read -r app; do
    [ "$app" = "busybox" ] && continue
    ln -sf busybox "$app"
  done < <(./busybox --list)
)

echo "make-rootfs: $ROOT ready ($(find "$ROOT"/bin -maxdepth 1 -type l | wc -l) applets)"
