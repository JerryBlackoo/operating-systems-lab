# RISC-V 架构三时刻自检

从仓库根目录运行：

```sh
make -C micro/kernel/arch/riscv run
```

也可以先进入本目录再执行 `make run`。需要 clang、ld.lld（或 Rust 工具链的 rust-lld）和 qemu-system-riscv64。自检 Makefile 定义 `-DARCH_SELFTEST=1`，只让这条路径在 S-mode 非法指令后按 4 字节指令假设前进；正式内核不定义该宏。生成物位于 `micro/build/riscv-arch-selftest/`，本目录的 `make clean` 只清除此自检的构建产物。

这是根据历史验收记录重建的独立自检，不是找回的原始临时 harness。它链接现有 `boot.S`、`context.S`、`trap.S`、`trap.c` 和 `sbi.c`，由 `selftest.c` 提供 `boot_main`。它不链接正式的 `kernel/boot/boot.c`，也不改变 `micro/Makefile` 的 `make run` 行为。

OpenSBI 输出之后，应看到 `[boot]`、`[ctx]`、`[trap]` 三组信息，最后输出 `[micro] all three moments verified, shutting down`，QEMU 正常退出。`trap.c` 在 S-mode 非法指令分支直接打印 `scause`/`sepc`/`stval`，`selftest.c` 返回后再读回 CSR 并打印 `expected`，用于核对 `sepc` 已按 `+4` 前进。本机 QEMU 8.2.2 实测的完整串口记录见 [`qemu-selftest.log`](qemu-selftest.log)。检查失败时会输出 `[selftest] FAIL`，并请求 QEMU 以非零状态退出。

自检覆盖启动栈与 BSS 探针、hart 的 tp、stvec 安装、内核 context 的 A→B→A 切换，以及 S-mode 写只读 CSR 所触发的非法指令异常和返回。它不验证 U-mode、定时器中断、MMU、多 hart 或从 trap 切到另一个线程。QEMU 执行的是模拟的 RISC-V 机器指令，不是物理 RISC-V CPU。
