#!/usr/bin/env bash
# Clone curated CPU / ISA / hardware learning repositories into external/cpu-learning/.
# Idempotent: skips repos that already exist.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${ROOT}/external/cpu-learning"
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

# --- RISC-V ISA simulators & reference models ---
clone_repo "https://github.com/riscv-software-src/riscv-isa-sim.git" spike
clone_repo "https://github.com/cvut/qtrvsim.git"
clone_repo "https://github.com/qemu/qemu.git"

# --- Educational CPU cores (Chisel / RTL) ---
clone_repo "https://github.com/ucb-bar/riscv-sodor.git"
clone_repo "https://github.com/chipsalliance/rocket-chip.git"
clone_repo "https://github.com/lowRISC/ibex.git"
clone_repo "https://github.com/openhwgroup/cva6.git"

# --- RISC-V software / test infrastructure ---
clone_repo "https://github.com/riscv-software-src/riscv-pk.git"
clone_repo "https://github.com/riscv-software-src/riscv-tests.git"
clone_repo "https://github.com/riscv/riscv-opcodes.git"

# --- HDL tooling ---
clone_repo "https://github.com/verilator/verilator.git"

# Symlink to NJU NEMU (already cloned under os-learning)
if [ ! -e nemu ] && [ -d "${ROOT}/external/os-learning/nemu/.git" ]; then
  ln -s ../os-learning/nemu nemu
  echo "link  nemu -> ../os-learning/nemu"
fi

echo "done. CPU learning repos under: $DEST"
