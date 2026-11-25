# kernel Directory Structure Details

> Parent Project: micro
> Document Location: codebase-docs/kernel-structure.md
> Related Main Document: codebase-structure.md

## Module Responsibility

The kernel tree provides the RISC-V machine bring-up (boot, trap, PLIC, SBI), the object/memory/scheduling/IPC/capability primitives, console I/O, and the resident storage bring-up that drives block and ext4 services from the boot path.

## Directory Structure

```
kernel/
├── arch/
│   └── riscv/              # RISC-V boot entry, trap save/restore, CSR/context helpers, PLIC and SBI glue
├── boot/                   # Kernel init sequence, BootInfo/FDT parsing, bootstrap plan, resident trap and service boot
├── capability/             # Capability table, endpoints, architecture hooks, self-test
├── clock/                  # Timebase, timer events, scheduling clock source
├── console/                # UART console output path
├── interrupt/              # IRQ registration/dispatch and interrupt-to-IPC notification bridge
├── ipc/                    # Endpoint messaging between kernel objects and services
├── loader/                 # ELF loader and user address-space setup
├── memory/                 # Physical frame allocator and address-space management
├── objects/                # Kernel object model and reference handling
├── sched/                  # Thread scheduling and RISC-V context switch
├── storage/                # Storage bring-up orchestration plus SHA-256 verification
└── syscall/                # Syscall dispatch, user backend, closure and U-mode probe programs
```

## Key Packages / Module Descriptions

- `kernel/boot`: owns the default-image initialization order; `kernel_init` wires console, PLIC, block, ext4, VFS, and resident idle loop.
- `kernel/arch/riscv`: owns trap entry/exit and external interrupt acknowledgement; `plic.c` programs priorities, enables sources, and claims/completes IRQs.
- `kernel/storage`: `storage_bringup` performs the boot-time block read, ext4 mount, `/motd` read, and hash verification used by M5 acceptance.
- `kernel/interrupt`: `irq.c` maps external IRQ numbers to registered handlers and forwards completion notifications.