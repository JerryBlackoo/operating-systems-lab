# G1 特权级、系统调用 ABI、最小用户态

- 王道：1.5 中断、异常和系统调用；1.6 运行环境
- 汤小丹：1.4 操作系统引导与运行；用户态/核心态、陷阱机制

## 实现

- `shared/abi/syscall.h`：编号、错误码、`open` 标志、`os_stat`。
- RISC-V：U-mode `ecall`，`trap_dispatch` 用 `a7` 选号，`a0`–`a2` 传参，返回值写回 `a0`。
- ARM：`svc #0` 入口已接入统一 ABI；`hello` 用户态经 SVC 跑通（`write/exit`）。
  注意 ARM 的 SVC 入口用 32 位寄存器转调 `arm_syscall_dispatch`，避免 64 位参数在 AAPCS 下错位。
- `userland/hello` 为固定地址 `0x40000000` 的 flat 镜像，经 `.incbin` 嵌入内核。
- `sys_write` 从用户页表逐页 `copyin`，不依赖内核直接解引用用户指针。

## 取舍

- 不做完整 libc、动态链接、ELF 重定位；用户程序用固定地址 flat 镜像。
- 非法系统调用打印一次 `[syscall] bad nr=...` 并返回 `OS_ENOSYS`。
- RISC-V 用户页表必须同时映射内核 RAM/UART，否则陷阱入口取指会失败。

## 验收

```bash
make && timeout 10s make run
```

RISC-V 与 ARM 日志都出现 `hello from user`；ARM 的 SVC 返回用户路径已实测。

## 已知限制

- ARM 已支持 `hello/fork/exec/shell` 的 SVC 路径；缺页和完整 IRQ 抢占仍未接入。
- 没有 `argv/envp`，`exec` 只按内嵌镜像名加载。
