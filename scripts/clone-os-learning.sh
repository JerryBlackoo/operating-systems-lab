#!/usr/bin/env bash
# Clone curated OS-from-scratch teaching repositories into external/os-learning/.
# Idempotent: skips repos that already exist.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${ROOT}/external/os-learning"
mkdir -p "$DEST"
cd "$DEST"

clone_repo() {
  local url="$1"
  local name="${2:-$(basename "$url" .git)}"
  if [ -d "$name/.git" ]; then
    echo "skip  $name (already cloned)"
    return 0
  fi
  echo "clone $name ..."
  git clone --depth 1 "$url" "$name"
}

# --- Dynux (no public source found as of 2026-08-14) ---
# If you have a private URL, add: clone_repo "https://github.com/<org>/dynux.git" dynux

# --- Cinux family (x86_64, modern C++, Chinese tutorial) ---
clone_repo "https://github.com/Awesome-Embedded-Learning-Studio/Cinux-Book.git"
clone_repo "https://github.com/Awesome-Embedded-Learning-Studio/Cinux.git"
clone_repo "https://github.com/Awesome-Embedded-Learning-Studio/Cinux-Base.git"
clone_repo "https://github.com/Charliechen114514/CCOperatingSystemX64.git"
clone_repo "https://github.com/Charliechen114514/CCOperateSystem.git"

# --- Classic step-by-step tutorials ---
clone_repo "https://github.com/cfenollosa/os-tutorial.git"
clone_repo "https://github.com/ruiers/os-tutorial-cn.git"
clone_repo "https://github.com/yourtion/30dayMakeOS.git"
clone_repo "https://github.com/StevenBaby/onix.git"

# --- University course labs ---
clone_repo "https://github.com/mit-pdos/xv6-riscv.git"
clone_repo "https://github.com/mit-pdos/xv6-public.git"
clone_repo "https://github.com/NJU-ProjectN/abstract-machine.git"
clone_repo "https://github.com/NJU-ProjectN/nanos-lite.git"
clone_repo "https://github.com/NJU-ProjectN/nemu.git"
clone_repo "https://github.com/NJU-ProjectN/navy-apps.git"
clone_repo "https://github.com/chyyuu/ucore_pub.git"
clone_repo "https://github.com/rcore-os/rCore-Tutorial-v3.git"

# --- Rust / other languages ---
clone_repo "https://github.com/phil-opp/blog_os.git"
clone_repo "https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials.git"

# --- Additional hobby / teaching kernels ---
clone_repo "https://github.com/SkiftOS/skift.git"
clone_repo "https://github.com/nanvix/nanvix.git"

echo ""
echo "Done. Repositories are under: $DEST"
ls -1 "$DEST"
