# CPU Learning Repositories

Curated clones of open-source CPU simulators, RTL cores, and RISC-V tooling for studying computer architecture and ISA implementation.

Re-clone or refresh everything:

```bash
bash scripts/clone-cpu-learning.sh
```

**Note:** `nemu` (NJU Project-N ISA emulator) is linked from `../os-learning/nemu` if you have already run `scripts/clone-os-learning.sh`.

## Cloned repositories

| Directory | Description |
|-----------|-------------|
| `spike` | [riscv-isa-sim](https://github.com/riscv-software-src/riscv-isa-sim) — official RISC-V ISA golden reference simulator |
| `qtrvsim` | Qt-based RISC-V pipeline simulator (GUI, Czech TU) |
| `qemu` | QEMU system emulator (multi-arch, includes TCG CPU translation) |
| `riscv-sodor` | Berkeley educational RISC-V cores (1/2/3/5-stage, Chisel) |
| `rocket-chip` | Generator for RISC-V SoCs (Rocket core, TileLink, Chisel) |
| `ibex` | lowRISC 2-stage / 3-stage embedded RISC-V core (SystemVerilog) |
| `cva6` | OpenHW CVA6 (formerly Ariane) 6-stage in-order RISC-V core |
| `riscv-pk` | RISC-V proxy kernel / boot loader (BBL) |
| `riscv-tests` | Official RISC-V ISA test suite |
| `riscv-opcodes` | RISC-V instruction encoding definitions |
| `verilator` | Fast Verilog/SystemVerilog simulator (for RTL cores) |
| `nemu` | Symlink to NJU NEMU (see `external/os-learning/nemu`) |

## Related (already under `external/os-learning/`)

| Path | Description |
|------|-------------|
| `os-learning/nemu` | NJU course CPU emulator (x86/MIPS/RISC-V) |
| `os-learning/linux/arch/x86` | Real x86 CPU bring-up, paging, traps (`arch-x86` symlink) |
| `os-learning/abstract-machine` | Bare-metal framework used with NEMU |

## Size notes

- `qemu` and `rocket-chip` are large even with `--depth 1` shallow clones.
- For full history of any repo: `git -C <name> fetch --unshallow`.
- `gem5` is intentionally omitted (multi-GB); add manually if needed.

These directories are gitignored in the parent repo; only this README and the clone script are tracked.
