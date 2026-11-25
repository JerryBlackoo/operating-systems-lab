# micro Codebase Document

> 📁 This document is a project overview. Modules with complex directory structures have been split into independent sub-documents — see section notes for details.

## 1. Tech Stack

- **Language standard**: Unknown (Makefile does not pass `-std=`; clang default applies)
- **Build system**: Make (top-level `Makefile`, module fragments in `shared/modules.mk`)
- **Compiler**: `clang` with `--target=riscv64-unknown-elf`, `-march=rv64imac_zicsr_zifencei -mabi=lp64 -mcmodel=medany` (from `Makefile > COMMON_CFLAGS`)
- **Linker**: `ld.lld` or `rust-lld` exposed as `ld.lld` (from `Makefile > LLD_REAL`)
- **Emulator**: `qemu-system-riscv64`, `-machine virt -m 128M -smp 1` (from `Makefile > QEMUFLAGS`)
- **Test framework**: Host-side self-test binaries plus QEMU boot demos driven by Make targets (from `Makefile > check`)
- **Core dependencies**: None (freestanding, `-ffreestanding -nostdlib`)

## 2. Project Directory Structure

```
micro/
├── kernel/                   # Kernel-resident code: boot, arch, object/memory/sched/IPC/capability, storage
│   # 📄 See: codebase-docs/kernel-structure.md
├── userland/                 # U-mode services and runtime: init, libs, servers (fs/block/vfs/drivers), stub
│   # 📄 See: codebase-docs/userland-structure.md
├── shared/                   # Cross-kernel common contracts: ABI constants, demo trap glue, module manifest
├── tools/                    # Host image builders and UART/IRQ test drivers (initrd, ext4 image)
├── docs/                     # Goals M0–M7, design notes, and acceptance evidence logs
├── build/                    # Build artifacts (kernel ELF, user ELFs, initrd, filesystem images) — not expanded
├── Makefile                  # Single-kernel build, image generation, QEMU run, host check entry points
└── README.md                 # Module status table, real-vs-stand-in boundaries, build instructions
```

## 3. Dev Commands (Core)

### Build Commands
```bash
# make clean all (Source: README.md > 构建与运行（微内核）)
make clean all

# default capability image build (Source: Makefile > all / $(KERNEL))
make
```

### Test Commands
```bash
# full host self-tests + standalone module ELFs + BOOT_MODULES=all QEMU run (Source: Makefile > check)
make -C micro check

# individual host self-tests (Source: Makefile > check)
make -C kernel/objects test
```

### Run / Start Commands
```bash
# default capability image with timer preemption probe (Source: README.md > 默认 QEMU 演示)
make -C micro run

# all 20 standalone module demos in sequence (Source: README.md > 默认 QEMU 演示)
make -C micro run BOOT_MODULES=all

# list module identifiers (Source: Makefile > list-modules)
make -C micro list-modules

# explicit filesystem image (Source: Makefile > FS_IMAGE / run)
make -C micro run FS_IMAGE=build/micro-root-m1.img
```

## 4. Dev Environment (Brief)

### Prerequisites
- clang (targeting `riscv64-unknown-elf`)
- ld.lld or rust-lld
- qemu-system-riscv64 (package `qemu-system-misc`)
- python3 (host image builders in `tools/`)

### Configuration Notes (Optional)
- `BOOT_MODULES` selects the demo/boot profile; default `capability` links the resident storage service chain.
- `FS_IMAGE` selects the ext4 image used by the QEMU virtio-blk device; default `build/micro-root.img`.
- `QEMU_TIMEOUT` defaults to `15s`.

## 5. Sub-document Index

| Document | Corresponding Module / Business Domain | Responsibility Summary |
|----------|---------------------------------------|------------------------|
| [kernel-structure.md](codebase-docs/kernel-structure.md) | `kernel/` | Boot, RISC-V arch/trap/PLIC, objects, memory, scheduling, IPC, capability, storage bring-up |
| [userland-structure.md](codebase-docs/userland-structure.md) | `userland/` | U-mode init/runtime, service servers, block/FS/driver stack, stubs |