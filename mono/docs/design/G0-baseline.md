# G0 基线冻结与教材映射

- 王道：1.1–1.6
- 汤小丹：第 1 章
- 基线日期：2026-11-19

## 实现取舍

- 只冻结启动、UART、异常/时钟、物理页分配四项能力，不提前实现进程或文件系统。
- 章节映射写入 `docs/roadmap.md`；Goal 正文入口统一到 `docs/goals/`。
- 基线日志保留在 `/tmp/oslab-g0-riscv.log` 与 `/tmp/oslab-g0-arm.log`；它们是“改动前”的证据，不回填后续能力。

## 验收

```bash
make && timeout 8s make run
make ARCH=arm && timeout 12s make ARCH=arm run
```

两架构均出现 `[boot] hello from the kernel` 与至少一次 `[clock] tick 1`。

## 已知限制

- 基线没有进程、调度、系统调用、MMU、文件系统。
- 实机 Z7-Lite 不在 G0 范围（见 G14）。
