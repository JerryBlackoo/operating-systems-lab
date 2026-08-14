# OS Learning Repositories

Curated clones of open-source projects that teach operating system development from scratch.

Re-clone or refresh everything:

```bash
bash scripts/clone-os-learning.sh
```

## Dynux

**No public `Dynux` OS source repository was found** on GitHub or Gitee (2026-08-14). The only GitHub match named `dynux` is [zignis/dynux](https://github.com/zignis/dynux), a Redux library—not an OS.

If `Dynux` is a private repo or lives elsewhere, add its URL to `scripts/clone-os-learning.sh` and re-run the script.

The closest public match to a full Chinese x86_64 teaching OS with milestone tags (MBR → GUI) is the **Cinux** family below.

## Cloned repositories

| Directory | Description |
|-----------|-------------|
| `Cinux-Book` | Stable teaching x86_64 OS (C++), MBR → GUI |
| `Cinux` | Active Cinux development line |
| `Cinux-Base` | Shared Cinux base components |
| `CCOperatingSystemX64` | Charliechen x86_64 from-scratch OS |
| `CCOperateSystem` | Older gcc/nasm/bochs OS tutorial |
| `os-tutorial` | cfenollosa step-by-step x86 tutorial (EN) |
| `os-tutorial-cn` | Chinese translation of os-tutorial |
| `30dayMakeOS` | 《30天自制操作系统》中文源码 |
| `onix` | Onix OS with Bilibili video series |
| `xv6-riscv` | MIT xv6 (RISC-V) |
| `xv6-public` | MIT xv6 (x86) |
| `AbstractMachine` | NJU Project-N bare-metal framework |
| `nanos-lite` | NJU Project-N teaching OS |
| `nemu` | NJU ISA emulator (course companion) |
| `navy-apps` | NJU user-space apps for nanos-lite |
| `ucore_pub` | Tsinghua uCore teaching kernel |
| `rCore-Tutorial-v3` | rCore Rust OS tutorial (v3) |
| `blog_os` | Writing an OS in Rust (phil-opp) |
| `rust-raspberrypi-OS-tutorials` | Rust OS on Raspberry Pi |
| `toaruos` | ToaruOS from-scratch x86 OS |
| `skift` | Skift OS (C++) |
| `nanvix` | Nanvix research/teaching OS |
| `linux` | Linux kernel upstream (`torvalds/linux`, shallow clone) |

These directories are gitignored in the parent repo; only this README and the clone script are tracked.
