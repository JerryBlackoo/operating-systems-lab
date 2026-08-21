# 从 2026 OS 国赛项目反推学习路线

这份路线的目标不是“把仓库看完”，而是把国赛项目拆成一条自己能重新实现的能力链。

# Stage 0：先具备最小系统基础

需要能解释：

- CPU privilege level；
- trap / interrupt；
- virtual memory；
- page table；
- process / thread；
- context switch；
- syscall；
- filesystem；
- lock / race condition。

如果这些概念只能背定义，直接看国赛内核会很痛苦。

---

# Stage 1：自己写一个最小内核

目标：

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

重点不是代码多，而是每一步都能回答：

```text
谁触发？
CPU 当前在哪个 privilege？
寄存器发生什么变化？
数据放在哪里？
下一步是谁执行？
```

---

# Stage 2：建立 Linux-like 抽象

加入：

- task/process；
- fd table；
- file object；
- VFS；
- inode/dentry；
- address space；
- VMA；
- mmap；
- fork/exec/wait。

这一步开始重点训练“跨子系统理解”。

例如自己画出：

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

---

# Stage 3：开始读比赛项目

建议顺序：

## A. Open Euler step by step

先学“真实 Linux 内核怎样被拆开讲”。

## B. 陶酉册 / 笑南梁队

看完整参赛内核如何逐渐补功能。

## C. 0xDEADBEEF

重点研究 final-test、VMA、scheduler state、CPU lifecycle 等后期问题。

## D. Eureka OS

重点研究：

```text
RISC-V64 / LoongArch64
        ↓
arch abstraction
        ↓
common kernel
```

理解“架构相关”和“架构无关”的边界。

---

# Stage 4：不要顺序读源码，要沿数据流读

例如研究 `read()`：

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

这样读源码会比“从 main.c 第 1 行看到最后一行”有效得多。

---

# Stage 5：专门读 bug 修复 commit

建立自己的 bug notebook，每个 bug 只记录六项：

```text
1. 现象
2. 输入/触发条件
3. 正确语义
4. 实际状态变化
5. 哪个不变量被破坏
6. 修复如何恢复不变量
```

例如：

```text
现象：munmap 后某地址仍可访问
触发：跨两个 VMA 的部分 unmap
正确语义：对应页和 VMA 区间都必须被更新
根因：只删 PTE，没有正确 split VMA
修复：先切 VMA，再 unmap PTE，并处理边界
```

这一步是从“会写功能”升级到“会做系统”的关键。

---

# Stage 6：做一个自己的国赛风格项目

建议选一个方向，不要同时做十个创新点。

## 路线 A：Linux-compatible teaching kernel

目标：

- RISC-V64；
- fork/exec/wait；
- mmap/VMA；
- VFS；
- pipe；
- signals；
- 50~100 个 syscall；
- regression tests。

## 路线 B：双架构内核

RISC-V64 + LoongArch64，主动设计 `arch/` 接口。

## 路线 C：AI workload × OS

例如：

- LLM page prefetch；
- KV cache memory tiering；
- MoE expert swapping；
- Agent memory cache。

要求所有优化都有 benchmark。

---

# Stage 7：最终验收标准

不要只问“能不能跑”。至少做四种测试：

## Correctness

功能结果正确。

## Compatibility

行为和 Linux/POSIX 语义一致。

## Stress

大量 fork、mmap、file、pipe、线程并发。

## Performance

有 baseline，并能解释瓶颈。

最终你应该能够面对一个失败测试，画出完整链路：

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
