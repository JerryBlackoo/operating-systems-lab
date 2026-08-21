# 2026 操作系统国赛一等奖级项目总结

> 这份总结不是把所有收录项目都当成一等奖。
>
> 截至 2026-08-21，部分公开比赛页面、搜索索引和仓库信息仍不同步，因此这里总结的是：**从 2026 全国赛公开项目、决赛级项目和可见提交中，可以抽象出来的一等奖级能力模型。**

# 一、一等奖项目真正比的是什么

如果只把操作系统国赛理解成“谁实现的 syscall 多”，会低估比赛难度。

一等奖级项目通常同时跨过五层：

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

也就是说：

- 一个功能能跑一次，不够；
- 大多数测试能过，不够；
- 复杂并发、异常输入、内存压力下仍正确，才开始接近成熟内核；
- 最后还需要性能、可维护性、架构适配、demo 和文档。

---

# 二、一等奖级内核通常具备的能力树

## 1. Boot / Architecture

最低层首先要解决：

```text
firmware / bootloader
        ↓
entry
        ↓
初始化栈
        ↓
页表 / MMU
        ↓
trap vector
        ↓
kernel main
```

一等奖级项目不会把这部分当成“只要能启动就结束”，还会把架构相关代码封装清楚。

像 Eureka OS 这种双架构思路特别值得学习：

```text
RISC-V64 ─┐
           ├── arch abstraction ──> common kernel
LoongArch ─┘
```

真正体现的是：

- 哪些东西必须 architecture-dependent；
- 哪些东西应该抽象成统一 kernel interface。

---

## 2. Trap / Interrupt / Syscall

典型路径：

```text
用户程序
  ↓ ecall / syscall instruction
trap entry
  ↓
保存用户上下文
  ↓
识别 trap cause
  ↓
syscall dispatcher
  ↓
具体内核子系统
  ↓
写回返回值
  ↓
恢复上下文
  ↓
sret / return to user
```

一等奖级实现需要处理的不只是正常 syscall：

- page fault；
- illegal instruction；
- timer interrupt；
- device interrupt；
- signal/异常语义；
- 嵌套状态和返回路径。

---

## 3. Virtual Memory

这是内核竞赛最容易从“看懂课本”进入“真正复杂”的地方。

完整问题不是只有 page table：

```text
physical page allocator
        ↓
page table
        ↓
address space
        ↓
VMA
        ↓
mmap / munmap / brk
        ↓
page fault
        ↓
copy-on-write / lazy allocation
        ↓
file-backed mapping
```

高水平项目经常在 final-test 阶段大量修：

- VMA overlap；
- split / merge；
- unmap 边界；
- permission；
- fork 后共享与复制；
- file mapping 生命周期。

这就是为什么 `0xDEADBEEF` 里出现 VMA 类提交很值得研究。

---

## 4. Process / Thread / Scheduler

课本里的 scheduler 往往只写：

```text
ready queue
→ choose task
→ context switch
```

真实内核要维护的是状态机：

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

再叠加：

- sleep/wakeup；
- wait；
- exit；
- timer preemption；
- SMP；
- CPU affinity；
- race condition；
- lock ordering。

所以优秀项目真正难的是 **状态转移正确性**。

---

## 5. VFS / File System

一个成熟参赛内核不会把文件系统写成“路径字符串 → 文件内容”。

更合理的抽象层次是：

```text
process
  ↓
fd table
  ↓
file object
  ↓
VFS
  ↓
dentry / inode
  ↓
具体 filesystem
  ↓
block/cache/device
```

高分项目通常还要兼容：

- cwd；
- relative path；
- mount；
- pipe；
- dup；
- stat；
- directory；
- permission；
- unlink / rename 等语义。

---

## 6. Linux Compatibility

很多国赛内核最后不是“自定义 API 能跑”，而是越来越像一个 Linux-compatible kernel。

因此 syscall 实现会逐渐从：

```text
实现 read/write/open
```

变成：

```text
Linux ABI 语义是否一致？
错误码是否一致？
边界条件是否一致？
结构体布局是否一致？
fork/exec/wait/mmap/futex 等组合行为是否一致？
```

这一步的复杂度远高于“函数写出来”。

---

# 三、功能挑战一等奖级项目的共同模式

功能挑战与“自己写内核”不同。

它更像：

```text
真实系统问题
  ↓
找到 OS 层瓶颈
  ↓
设计机制
  ↓
实现 prototype
  ↓
benchmark
  ↓
证明比 baseline 好
```

例如：

## LLM 内存管理

```text
模型权重 / KV / MoE expert
        ↓
访问局部性变化
        ↓
内存不够
        ↓
page eviction / prefetch
        ↓
减少 stall
        ↓
提高 token throughput
```

真正需要证明：

- baseline 是什么；
- workload 是什么；
- 命中率/缺页率发生什么变化；
- latency / throughput 提升多少；
- 额外 CPU / memory overhead 多大。

## Agent Memory

可能形成：

```text
Agent 运行
 ↓
大量上下文 / memory 对象
 ↓
hot / cold 分化
 ↓
缓存、淘汰、持久化
 ↓
减少读取开销
```

一等奖级区别依然在：**不是“有这个想法”，而是机制闭环 + 数据证明。**

---

# 四、为什么 final-test 阶段的 commit 最值得看

如果要学习真正的工程能力，我最建议看最后 20% 的提交。

因为前 80% 经常是：

```text
功能从 0 → 1
```

后 20% 才是：

```text
为什么这个边界 case 会炸？
为什么并发时才炸？
为什么只在某个架构炸？
为什么跑 1000 次才炸一次？
为什么功能正确但性能掉 10 倍？
```

这些 commit 才是系统能力的核心。

建议每个仓库都按这个流程读：

```bash
git log --oneline --all

git log --grep='fix\|test\|final\|bug\|race\|vma\|sched\|syscall' --oneline

git show <commit>
```

然后自己写出：

```text
症状
→ 触发条件
→ 根因
→ 哪个数据结构状态错了
→ 修复方案
→ 为什么修复有效
```

---

# 五、一等奖与普通课程 OS 的真正差距

课程 OS 通常让你实现：

- page table；
- scheduler；
- syscall；
- file system。

一等奖级竞赛 OS 面对的是：

```text
这些模块同时存在以后，彼此之间会发生什么？
```

例如：

```text
fork
 ↓
复制 address space
 ↓
VMA / page table / COW
 ↓
继承 fd table
 ↓
继承 cwd
 ↓
新 task 进入 scheduler
 ↓
父子 wait/exit
```

一个 `fork()` 就横跨：

- process；
- memory；
- VFS；
- scheduler；
- syscall ABI。

所以一等奖本质上不是“每个知识点会不会”，而是：

> **能不能把整个系统重新拼起来，并保证状态始终一致。**

---

# 六、从这些项目抽象出来的 8 个一等奖能力

## 1. 系统分解能力

把复杂问题拆成明确子系统，而不是堆代码。

## 2. 状态机思维

永远问：

```text
当前状态是什么？
事件是什么？
事件之后谁修改状态？
谁还能看到这个状态？
```

## 3. 数据流追踪

例如：

```text
read(fd)
→ fd table
→ file
→ VFS
→ inode
→ filesystem
→ buffer/page cache
→ device
```

## 4. 并发思维

不是只有 lock API，而是：

```text
共享对象是谁？
谁会同时修改？
临界区边界在哪里？
锁顺序是什么？
```

## 5. Debug 能力

能从 crash / wrong result / hang / race 回溯到真正的状态不变量。

## 6. Compatibility 思维

不是“我觉得这样也可以”，而是：

```text
Linux/POSIX 语义规定是什么？
测试程序依赖了什么？
```

## 7. Benchmark 思维

性能优化必须有：

```text
baseline
→ measurement
→ bottleneck
→ change
→ measurement again
```

## 8. 工程化能力

包括：

- 分支管理；
- 小步 commit；
- regression test；
- CI；
- 文档；
- demo；
- issue / bug tracking。

---

# 七、最值得复制的不是代码，而是研发方法

真正值得从一等奖项目复制的是这个循环：

```text
读规范 / 测例
      ↓
建立系统模型
      ↓
实现最小机制
      ↓
运行测试
      ↓
记录失败
      ↓
追踪数据流与状态变化
      ↓
找到不变量被破坏的位置
      ↓
修复
      ↓
回归测试
      ↓
性能测试
      ↓
重构
```

这套方法可以直接迁移到：

- Linux kernel；
- database；
- compiler；
- distributed systems；
- AI systems。

---

# 八、如果自己参加下一届，应该做到什么程度

## Level 1 — 教学 OS

能自己实现：

- boot；
- trap；
- paging；
- scheduler；
- syscall；
- simple FS。

## Level 2 — Linux-like kernel

加入：

- mmap/VMA；
- fork/exec/wait；
- pipe；
- signals；
- richer VFS；
- SMP 基础。

## Level 3 — 国赛可测试

加入：

- Linux syscall compatibility；
- final-test regression；
- stress test；
- race debugging；
- benchmark。

## Level 4 — 一等奖竞争力

至少再有一个突出方向：

- 多架构；
- 极强兼容性；
- SMP / scheduler；
- 内存性能；
- 文件系统；
- AI workload；
- eBPF / observability；
- virtualization；
- security。

最终目标应该是：

```text
不是“我写过一个 OS”
而是
“给我一个失败测例，我能沿着整个系统找到为什么失败。”
```
