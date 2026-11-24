# [G1] 特权级、系统调用 ABI、最小用户态载入

**依赖**：G0  
**建议 Agent**：A1

## 背景

内核可 S-mode / SVC 运行，尚无用户态与统一 syscall 分发。

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 1.5 中断、异常和系统调用 | 1.4 操作系统引导与运行 |
| 1.6 操作系统的运行环境 | 用户态/核心态、陷阱机制 |

## 目标

1. `shared/abi/`：`syscall.h` 定义号、`stat`、最小 `write/read/exit`。
2. RISC-V：`ecall` → `syscall_dispatch`；ARM：`SVC` 统一入口。
3. `userland/hello`：裸链接或 flat 镜像，由内核加载到固定用户入口；`write(1, "hello\n", 6)` 经 syscall。
4. `docs/design/G1-privilege-syscall.md`：双态切换步骤对照教材图。

## 交付文件（示例路径，可调整但须更新 design 文档）

- `shared/abi/syscall.h`
- `kernel/syscall/syscall.c`
- `kernel/proc/user_load.c`（或等价）
- `userland/hello/*.S` + 链接脚本
- `Makefile` 目标：`userland` 产物并入 `make`

## 验收

```bash
make && make run
# 期望除内核 boot 日志外，出现用户态：hello from user
```

非法 syscall 号：返回明确负数错误码并打印一次 `[syscall] bad nr=...`（不 panic 整机除非配置为 strict）。

## 非目标

- 完整 libc、动态链接
- ELF 完整加载器重定位全套（可做最小 ET_EXEC 固定地址）

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。