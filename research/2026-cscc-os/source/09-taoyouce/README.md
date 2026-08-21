# RustMicroOS

> **2026 年全国大学生计算机系统能力大赛 - 操作系统设计赛**  
> **内核实现赛道**

<p align="center">
  <b>一个使用 Rust 从零构建的 RISC-V × LoongArch 双架构操作系统内核</b><br>
  <i>面向 oscomp 全量测评、对标 Linux 语义、以确定性指令计数（icount）驱动性能优化</i>
</p>

---

| 项目信息 | |
|---------|---|
| **高校名称** | 合肥工业大学 |
| **队伍 ID** | 35873 |
| **队伍名** | 陶西册 |
| **选题方向** | 内核实现 |
| **项目名称** | RustMicroOS |
| **目标架构** | riscv64gc · loongarch64 |

> **关于名称**：项目沿用早期名称 `RustMicroOS`，但**当前架构已不是微内核**。
> 为通过竞赛的 11 个评测组（lmbench / LTP / busybox 等），内核已演进为一个
> **功能完整、对标 Linux 语义的宏内核式操作系统内核**：内置 VFS、网络栈、
> 完整信号/进程/线程模型、314 个系统调用、双指令集支持。下文以真实状态为准。

---

## 项目简介

RustMicroOS 是一个用 Rust 从零构建的操作系统内核，**同一份源码经 HAL 抽象同时编出 RISC-V 与 LoongArch 两个内核**，
在官方评测容器中跑通 oscomp 全部 11 个评测组（musl + glibc 双变体）。项目的核心工程方法是
**用确定性指令计数（QEMU icount）做可复现的 A/B 测量来指导真实的性能优化**——结构性地削减指令、锁与拷贝，
而非依赖随宿主负载抖动的 wall-clock 噪声。

> 上述 11 组属于前期兼容性回归背景。当前唯一正式靶心是 **FINAL-2026 glibc CAgent + BuildStorm**，
> 其状态和完成条件见下一节，不用历史成绩替代决赛结论。

### 规模一览

| 指标 | 数值 |
|------|------|
| 内核源码 | `kernel/src` 下 **264 个 Rust 文件** |
| Linux ABI | **300+ 系统调用面**，覆盖进程、内存、VFS、网络、信号与现代扩展 |
| 目标架构 | **2 个**：riscv64gc（Sv39 / virtio-mmio）、loongarch64（DMW+TLB / virtio-pci） |
| FINAL-2026 CAgent | **RV 10/10、LA 10/10，均为 199.1/200** |

---

## FINAL-2026 状态

状态快照：**2026-08-12**。权威规则来自
`reference-works/testsuits-for-oskernel-final-2026/`，原始证据来自
`local-judge/reports/`。

| 正式项目 | RISC-V64 | LoongArch64 | 当前结论 |
|---|---|---|---|
| CAgent glibc | **10/10 · 199.1/200** | **10/10 · 199.1/200** | 已收口 |
| BuildStorm toolchain | 最新正式轮 `exit=139` | 最新正式轮出现 `BUILDSTORM_TOOLCHAIN ok` | 继续复验 |
| BuildStorm compile | 尚无正式成功 marker | 尚无正式成功 marker | 攻坚中 |
| 原版官方全量 + judge | 未完成 | 未完成 | **不能宣布 BuildStorm 完成** |

BuildStorm 正式资源固定为 `SMP=8 MEM=8G`。调试内核、单阶段、单核或缩内存结果只用于
定位；只有双架构均出现完整成功 marker、`[SINGLE] END ... exit=0`，再由官方 judge
生成新鲜分数，才构成最终交付。

阶段性答辩材料采用可复现的 Marp + 视频脚本流水线：

- [正式答辩 PPT 源文件](doc/presentation/final-2026/slides/defense.md)
- [五分钟视频幻灯源文件](doc/presentation/final-2026/slides/video.md)
- [PPT / 视频生成说明](doc/presentation/final-2026/README.md)
- [BuildStorm 设计与优化记录](doc/design/buildstorm-final-2026.md)

---

## 系统架构

```
┌──────────────────────────────────────────────────────────────┐
│  用户态测评负载  basic busybox lua lmbench iozone libcbench    │
│                  cyclictest iperf netperf libctest LTP         │
├──────────────────────────────────────────────────────────────┤
│  系统调用层 · 314 syscalls · ELF 加载 / exec · 信号 / futex    │
├──────────────────────────────────────────────────────────────┤
│  核心子系统                                                    │
│   mm   Buddy / Slab / 页表 / 页缓存(LRU O(log n)) / 按需分页   │
│   sched  MLFQ 多级反馈队列 · RT 抢占                           │
│   fs   VFS / RamFS / ProcFS / ext4 / FAT32 / 共享页缓存        │
│   net  smoltcp TCP/UDP 栈                                      │
│   ipc / io_uring / async_rt / 信号 / 命名空间 / cgroupfs       │
├──────────────────────────────────────────────────────────────┤
│  安全 · 可观测   capability(seL4 风格) landlock seccomp        │
│                  eBPF · GDB stub · livepatch                   │
├──────────────────────────────────────────────────────────────┤
│  HAL 架构抽象层  hal_trait.rs                                  │
│     arch/riscv64  ·····  arch/loongarch64                      │
└──────────────────────────────────────────────────────────────┘
```

通过 `kernel/src/arch/hal_trait.rs` 抽象 MMU、陷入、设备与控制台差异，上层子系统零架构耦合——
**每修一处同时惠及两条 ISA**。

---

## 双架构支持

| | RISC-V (`kernel-rv`) | LoongArch (`kernel-la`) |
|---|---|---|
| 分页 | Sv39 三级页表 | DMW 直接映射窗口 + TLB refill |
| 设备 | virtio-mmio | virtio-pci |
| 陷入 | scause / stvec / SUM | ecfg / estat / 例外向量 |
| 浮点/向量 | F/D | FPU / LSX / LASX |
| 串口 | NS16550 @ 0x10000000 | 16550 兼容 |

---

## 兼容性回归背景

在进入 FINAL-2026 前，项目长期以
`rv-musl / rv-glibc / la-musl / la-glibc` 四元组回归 basic、busybox、Lua、
libctest、LTP、lmbench、iozone、网络与实时测试。它们用于守住 Linux ABI 兼容性，
但不是当前决赛得分。

历史批次、原始串口与当时口径统一保留在：

- `docs/BATCH-HISTORY.md`
- `local-judge/logs/`
- `local-judge/reports/`
- `doc/scores/`

对外材料不再复制易过期的总分表；需要引用具体数字时，必须同时给出批次、RunId 和原始日志。

---

## 性能优化亮点（icount 实证）

所有优化均**语义等价、零功能回归**，先用 icount 隔离实证"改前 vs 改后"真减少了工作量，再跑四元组回归后并入主线。

| 优化 | 效果 |
|------|------|
| ASID 分配器 O(65536) 死扫描移除 | 线程退出 613K→27K 指令（**−95.6%**） |
| timer / IO 唤醒 1..NEXT_PID 全扫 → 只遍历真实进程 | createjoin O(N²)→O(N)，**2.09×** 平坦化 |
| 合并 enter/exit accounting | Simple syscall 4.67→3.43µs（**−26.5%**）；getppid/read/write 各 **−41~52%** |
| unlink 三次路径 walk → 一次 | lat_fs 文件删除吞吐 **8~10×** |
| readahead 1→32 页 | la-glibc lmbench 0→48.83（解开 HUNG） |
| 页缓存 LRU VecDeque → BTreeMap | O(n)→**O(log n)**，根治"跑越久越慢" |
| fork+execve ENOEXEC 修复 | 282287→1099µs（**−99.6%**） |
| MAP_SHARED 按需分页 + msync(MS_INVALIDATE) 驱逐 | lat_pagefault 对标 Linux 缺页语义 |

### 方法论：icount 确定性测量

裸 QEMU 的虚拟时钟随宿主负载抖动，同一内核每次跑分都不同。给 QEMU 加
`-icount shift=N,sleep=off` 后，虚拟时钟变成"已执行 guest 指令数"的确定函数 →
**可复现、与机器快慢无关的干净 A/B**。详见 `local-judge/scripts/run-single-icount.sh`。

> icount 仅用于**指导优化的正当测量**；官方仍以 wall-clock 计分。结构性砍指令/锁/拷贝在任何机器上都更快——这才是抗波动的真功夫，绝不伪造上报值。

---

## 快速开始

### 环境要求

- 官方评测容器：`zhouzhouyi/os-contest:20260510`
- 热容器：`osdev`，源码 `/workspace`，内核与正式镜像 `/artifacts`
- PowerShell 7（Windows 调度）与 Bash（容器内脚本）

### 双架构构建

```bash
# 在官方热容器中构建 release + LTO 双架构内核
bash local-judge/scripts/in-docker.sh make all
```

### FINAL-2026 BuildStorm 阶段验证

```bash
# toolchain → minibuild → prebuild → compile → 原版官方全量
bash local-judge/scripts/final-2026/run-buildstorm-ladder.sh
```

正式流程只接受 `/artifacts/final-2026/pristine/sdcard-{rv,la}-pub.img`，镜像须校验、
权限 `0444` 且以 `snapshot=on` 运行。阶段入口与完成条件见
[`local-judge/scripts/final-2026/README.md`](local-judge/scripts/final-2026/README.md)。

### 生成答辩 PPT

```powershell
pwsh -File doc/presentation/final-2026/render.ps1
```

---

## 目录结构

```
RustMicroOS/
├── kernel/src/
│   ├── arch/              # hal_trait.rs + riscv64/ + loongarch64/
│   ├── mm/               # 内存管理（buddy/slab/页表/页缓存/地址空间）
│   ├── sched/            # MLFQ 调度器
│   ├── fs/               # VFS / RamFS / ProcFS / ext4 / FAT32
│   ├── net/              # smoltcp 网络栈
│   ├── syscall/          # 300+ Linux ABI 调用面（nr.rs + 各模块）
│   ├── ipc/ io_uring/ async_rt/ capability/ security/ ...
├── user/src/bin/init.rs  # PID 1、评测调度与 SINGLE_CMD
├── local-judge/          # FINAL-2026、历史回归、icount 与原始报告
├── reference-works/      # 官方题面、脚本与 judge
├── docs/                 # 批次事实账本与工程分析
└── doc/                  # 设计、使用、答辩 PPT 与视频流水线
```

---

## 许可证

MIT OR Apache-2.0
