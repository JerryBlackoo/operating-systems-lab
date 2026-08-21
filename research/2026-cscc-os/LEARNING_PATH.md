# 从 2026 OS 国赛项目反推学习路线

这份路线的目标不是“把仓库看完”，而是把国赛项目拆成一条自己能重新实现的能力链。

## Stage 0：最小系统基础

需要能解释：CPU privilege level、trap/interrupt、virtual memory、page table、process/thread、context switch、syscall、filesystem、lock/race condition。

## Stage 1：自己写一个最小内核

```text
boot
→ UART 输出
→ trap
→ timer
→ physical allocator
→ page table
→ user mode
→ syscall
→ scheduler
```

每一步都回答：谁触发？CPU 在哪个 privilege？寄存器怎么变化？数据放哪里？下一步谁执行？

## Stage 2：建立 Linux-like 抽象

加入：task/process、fd table、file object、VFS、inode/dentry、address space、VMA、mmap、fork/exec/wait。

例如自己画：

```text
fork()
├─ task
├─ scheduler
├─ mm
├─ VMA
├─ page table
├─ fd table
└─ parent/child relation
```

## Stage 3：开始读比赛项目

建议顺序：

1. **Open Euler step by step**：先学真实 Linux 内核怎样被拆开讲。
2. **陶酉册 / 笑南梁队**：看完整参赛内核怎样逐渐补功能。
3. **0xDEADBEEF**：重点研究 final-test、VMA、scheduler state、CPU lifecycle。
4. **Eureka OS**：重点研究 RISC-V64 / LoongArch64 的 arch abstraction。

## Stage 4：沿数据流读源码

研究 `read()`：

```text
user read()
→ syscall instruction
→ trap entry
→ syscall table
→ sys_read
→ fd table
→ file
→ VFS
→ filesystem
→ cache/device
→ return
```

研究 `fork()`：

```text
sys_fork
→ allocate task
→ copy registers
→ clone address space
→ copy/share VMA
→ duplicate fd refs
→ enqueue scheduler
→ parent/child return values
```

研究 page fault：

```text
CPU translation failure
→ trap
→ fault address
→ find VMA
→ permission check
→ allocate/load page
→ update PTE
→ TLB handling
→ retry instruction
```

## Stage 5：专门读 bug 修复 commit

建立 bug notebook，每个 bug 只记录六项：

```text
1. 现象
2. 输入/触发条件
3. 正确语义
4. 实际状态变化
5. 哪个不变量被破坏
6. 修复如何恢复不变量
```

## Stage 6：做一个自己的国赛风格项目

### 路线 A：Linux-compatible teaching kernel

目标：RISC-V64、fork/exec/wait、mmap/VMA、VFS、pipe、signals、50~100 个 syscall、regression tests。

### 路线 B：双架构内核

RISC-V64 + LoongArch64，主动设计 `arch/` 接口。

### 路线 C：AI workload × OS

例如 LLM page prefetch、KV cache memory tiering、MoE expert swapping、Agent memory cache；所有优化都必须有 benchmark。

## Stage 7：最终验收

至少做四种测试：

- **Correctness**：功能结果正确。
- **Compatibility**：行为和 Linux/POSIX 语义一致。
- **Stress**：大量 fork、mmap、file、pipe、线程并发。
- **Performance**：有 baseline，并能解释瓶颈。

最终应该能面对一个失败测试，画出完整链路：

```text
测试输入
→ syscall
→ 内核对象
→ 数据结构变化
→ 调度/内存/文件系统交互
→ 错误状态
→ 根因
→ 修复
```

到这一步，才真正接近国赛高水平项目训练出来的能力。
