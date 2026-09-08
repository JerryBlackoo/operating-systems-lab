#!/usr/bin/env bash
# Idempotent dependency + generated-state setup for the ctr container-runtime lab.
# Runs after checkout (at build time when environment builds are enabled).
set -euo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

# System package used to assemble the busybox test rootfs. Stable + small.
if [ ! -x /usr/bin/busybox ]; then
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq busybox-static
fi

# Generate ./busybox-rootfs for tests/run_tests.sh (idempotent).
bash .cursor/make-rootfs.sh busybox-rootfs

# Fetch Go module dependencies once the module is initialized. Guarded so this
# install command is also safe in the current pre-implementation state (no go.mod).
if [ -f go.mod ]; then
  go mod download
fi

echo "install.sh: done — go $(go version | awk '{print $3}'), busybox $(/usr/bin/busybox | head -1 | awk '{print $2}')"
