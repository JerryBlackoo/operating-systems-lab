# 2026 操作系统国赛一等奖级项目总结

> 这份总结不是把所有收录项目都当成一等奖。
>
> 截至 2026-08-21，公开比赛页面、搜索索引和仓库信息并不完全同步，因此这里总结的是：**从 2026 全国赛公开项目、决赛级项目和可见提交中，可以抽象出来的一等奖级能力模型。**

## 1. 一等奖项目真正比的是什么

如果只把操作系统国赛理解成“谁实现的 syscall 多”，会低估比赛难度。一等奖级项目通常同时跨过五层：

```text
正确性
  ↓
完整性
  ↓
兼容性
  ↓
稳定性
  ↓
性能与工程化
```

一个功能能跑一次不够；复杂并发、异常输入、内存压力下仍正确，才开始接近成熟内核。最后还需要性能、可维护性、架构适配、demo 和文档。

## 2. Boot / Architecture

典型启动链：

```text
firmware / bootloader
→ entry
→ stack
→ page table / MMU
→ trap vector
→ kernel main
```

一等奖级项目会明确区分架构相关与架构无关代码。像 Eureka OS 的双架构方向最值得研究：

```text
RISC-V64 ─┐
           ├── arch abstraction ──> common kernel
LoongArch ─┘
```

## 3. Trap / Interrupt / Syscall

```text
user program
→ syscall instruction
→ trap entry
→ save context
→ identify cause
→ syscall dispatcher
→ kernel subsystem
→ return value
→ restore context
→ return to user
```

高水平实现还要处理 page fault、illegal instruction、timer/device interrupt、signal/异常语义和复杂返回路径。

## 4. Virtual Memory

真正完整的问题是：

```text
physical page allocator
→ page table
→ address space
→ VMA
→ mmap / munmap / brk
→ page fault
→ COW / lazy allocation
→ file-backed mapping
```

final-test 阶段最容易暴露 VMA overlap、split/merge、unmap 边界、permission、fork 后共享/复制、file mapping 生命周期等问题。

## 5. Process / Thread / Scheduler

课本常写成：

```text
ready queue → choose task → context switch
```

真实内核维护的是状态机：

```text
NEW
 ↓
READY ←────────────┐
 ↓                 │
RUNNING ──yield────┘
 ↓  ↓
BLOCKED  ZOMBIE
 ↓
READY
```

再叠加 sleep/wakeup、wait/exit、timer preemption、SMP、CPU affinity、race condition、lock ordering。优秀项目真正难的是 **状态转移正确性**。

## 6. VFS / File System

成熟参赛内核更接近：

```text
process
→ fd table
→ file object
→ VFS
→ dentry / inode
→ concrete filesystem
→ cache / block / device
```

还要兼容 cwd、relative path、mount、pipe、dup、stat、directory、permission、unlink/rename 等语义。

## 7. Linux Compatibility

很多国赛内核最后不是“自定义 API 能跑”，而是越来越像 Linux-compatible kernel。真正困难的是：

```text
Linux ABI 语义是否一致？
错误码是否一致？
边界条件是否一致？
结构体布局是否一致？
fork/exec/wait/mmap/futex 等组合行为是否一致？
```

## 8. 功能挑战一等奖级项目的模式

功能挑战更像：

```text
真实系统问题
→ 找到 OS 层瓶颈
→ 设计机制
→ 实现 prototype
→ benchmark
→ 证明优于 baseline
```

例如 LLM 内存管理：

```text
模型权重 / KV / MoE expert
→ 访问局部性变化
→ 内存压力
→ eviction / prefetch
→ 减少 stall
→ 提高吞吐量
```

Agent Memory 也类似：围绕 hot/cold memory、cache、eviction、持久化和延迟做系统机制闭环。

## 9. 为什么 final-test 阶段的 commit 最值得看

前 80% 往往是功能从 0 → 1；后 20% 才开始回答：

```text
为什么这个边界 case 会炸？
为什么并发时才炸？
为什么只在某个架构炸？
为什么跑 1000 次才炸一次？
为什么功能正确但性能掉 10 倍？
```

建议这样读：

```bash
git log --oneline --all
git log --grep='fix\|test\|final\|bug\|race\|vma\|sched\|syscall' --oneline
git show <commit>
```

然后自己写：

```text
症状
→ 触发条件
→ 根因
→ 哪个数据结构状态错了
→ 修复方案
→ 为什么修复有效
```

## 10. 一等奖与普通课程 OS 的真正差距

课程 OS 往往分别实现 page table、scheduler、syscall、filesystem；国赛高水平项目面对的是：

> **这些模块同时存在以后，彼此之间会发生什么？**

例如 `fork()` 会同时触及 process、scheduler、mm、VMA、page table、fd table、parent/child relation。真正难的是保证跨子系统状态始终一致。

## 11. 八个一等奖级能力

1. **系统分解能力**：把复杂问题拆成明确子系统。
2. **状态机思维**：始终知道当前状态、事件和状态转移。
3. **数据流追踪**：能沿 syscall/VFS/mm/scheduler 追到底。
4. **并发思维**：知道共享对象、临界区和锁顺序。
5. **Debug 能力**：从 crash/hang/race 回溯到被破坏的不变量。
6. **Compatibility 思维**：以 Linux/POSIX 语义和 ABI 为准。
7. **Benchmark 思维**：baseline → measurement → bottleneck → change → measurement。
8. **工程化能力**：branch、commit、regression test、CI、文档、demo。

## 12. 最值得复制的是研发方法

```text
读规范 / 测例
→ 建立系统模型
→ 实现最小机制
→ 运行测试
→ 记录失败
→ 追踪数据流和状态变化
→ 找到不变量被破坏的位置
→ 修复
→ 回归测试
→ 性能测试
→ 重构
```

这套方法可以直接迁移到 Linux kernel、database、compiler、distributed systems、AI systems。

## 13. 如果自己参加下一届

### Level 1 — 教学 OS

- boot
- trap
- paging
- scheduler
- syscall
- simple FS

### Level 2 — Linux-like kernel

- mmap/VMA
- fork/exec/wait
- pipe
- signals
- richer VFS
- SMP 基础

### Level 3 — 国赛可测试

- Linux syscall compatibility
- final-test regression
- stress test
- race debugging
- benchmark

### Level 4 — 一等奖竞争力

再至少有一个突出方向：多架构、极强兼容性、SMP/scheduler、内存性能、文件系统、AI workload、eBPF/observability、virtualization 或 security。

最终目标不是“我写过一个 OS”，而是：

> **给我一个失败测例，我能沿着整个系统找到为什么失败。**
