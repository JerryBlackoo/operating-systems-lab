# micro：RISC-V 微内核实验树

与仓库内 [`mono/`](../mono/)（宏内核 G0–G14）并列。本树用**能力 + IPC + 独立地址空间**拼系统，不把策略堆进内核。

**目标与波次**：[`docs/goals/README.md`](docs/goals/README.md)（M0–M7）  
**全局合同**：[`docs/goals/GLOBAL-CONTRACT.md`](docs/goals/GLOBAL-CONTRACT.md)

## 系统里有哪些对象

微内核只有五个内核对象；其余都是用户态概念。

| 内核对象 | 定义 | 谁创建 | 寿命由谁终结 |
| --- | --- | --- | --- |
| **Object** | `kernel/objects/object.h` | 各类型模块 | `object_unref` 归零 → `destroy` |
| **Thread** | `kernel/sched/sched.h` | `thread_create` | 最后一次引用释放（`reap_dead` 在安全点回收） |
| **AddressSpace** | `kernel/memory/memory.h` | `as_create` | `as_unref` 归零 → 递归释放页表 |
| **Endpoint** | `kernel/ipc/ipc.h` | `ep_create` | `ep_unref` 归零 → 取消在途 call |
| **Reply** | `kernel/ipc/ipc.h` | `ep_call_open` / `reply_alloc` | 回复或取消后归还槽位 |

**没有内核对「进程」的定义**。`Process = AddressSpace + Thread + CapSpace`
是用户态 `pm` 服务的捆绑概念；`Process` 表在 PM 的静态数组里。

围绕这五个对象的三层结构：

```text
┌─ 内核（S-mode）─────────────────────────────────────────┐
│  arch/riscv    boot    objects  memory  sched  clock     │
│                        ipc      interrupt  capability    │
│                        syscall  loader     console       │
│                        storage(block + virtio_blk)       │
│                        pager                             │
└───────────────────────┬─────────────────────────────────┘
                        │ ecall（ABI）+ IPC + 缺页
┌─ U-mode ──────────────┴─────────────────────────────────┐
│  init ──spawn──> sm / pm / vm / fs / console / shell     │
│  libs（所有 U-mode 程序的唯一 syscall 入口）             │
│  fs 内部：ext4 + vfs（A1 之后不再在内核）                │
└─────────────────────────────────────────────────────────┘
```

## 当前状态（2026-11-25）

| 层次 | 状态 |
| --- | --- |
| 启动 | `kernel/boot` + `arch/riscv`：BootInfo、自举清单、`shared/demo.c` trap glue |
| 内核模块 | boot(FDT) / loader / objects / memory / sched / ipc / interrupt 有 host 自测；capability / clock / syscall 无独立 host `test` 目标，由单模块 QEMU demo 与整内核镜像覆盖 |
| 用户态模型 | init / libs + U-mode sm / pm / fs / console / shell（M7 六服务）；M6 另含内核拉起 `vm` / `heaptest` / `heaptest2`（initrd 9 成员） |
| **存储** | **策略层在 U-mode**（A1）：`ext4` + `vfs` 编进 `fs.elf`；内核只保留 `block` + `virtio_blk`，经 `ABI_BLOCK_READ`/`ABI_BLOCK_INFO` 暴露原始块 |
| **系统** | 默认 `BOOT_MODULES=capability`：内核常驻 → 内核 spawn `vm`/heaptest + init 分两阶段经 PM 拉 `fs/console` 然后 `shell` → virtio→ext4→VFS `/motd`。`make demo` / `tools/demo.sh` / `m5`/`m6` 可绿 |

## 演进主线

七个里程碑各自解决一个问题，**每一层都建立在前一层之上**：

| 里程碑 | 解决的问题 | 留下来的对象 |
| --- | --- | --- |
| **M0** 基线 | `make check` 能重复跑：host 单测 + 13 个单模块镜像 + 整内核清单 | `bootstrap_plan[]`、证据目录约定 |
| **M1** 常驻内核 | 内核跑完 demo 不退出 | 帧池、内核 AS、idle 线程 |
| **M2** initrd + ELF | 用户程序从哪来 | MIRD 容器、`loader_spawn_user`、launch 槽 |
| **M3** SM/PM/IPC | 服务怎么互相找到并调用 | `struct sm`、`struct pm`、`struct endpoint`、`struct reply` |
| **M4** 控制台中断 | 真实 stdin | UART IRQ 行队列、`irq_port` |
| **M5** virtio→ext4 | 真实文件系统 | `block_service`、`struct ext4`、`struct vfs` |
| **M6** VM 服务 | 缺页策略离开内核 | `pager`、`ABI_VM_MAP`、`struct vm` |
| **M7** 集成 | 六服务 + shell 交互 | `init` 分阶段启动、`shell`、端到端验收 |

M0–M7 之后的工作全部是**收敛与硬化**，不再新增里程碑：

| 阶段 | 修的东西 |
| --- | --- |
| 身份与权限 | A2（AS cap 链）、A3（SM 不信自报 sid）、A10（reply 归属）、A19（VM forget 身份） |
| 资源泄漏 | A5（reply 槽位）、A13（页表页）、A14/A17（VM 区间与驻留页）、A16（pager 超时） |
| 生命周期 | A11（线程销毁时清 IPC 队列）、A12（launch 槽归还时机）、A7（异步 park 保护） |
| 架构 | **A1（存储策略迁出内核）**、A1b（U-mode 栈 4 KiB→16 KiB） |
| 构建与可观测 | A6（配置开关不再污染 `.o`）、A31（分阶段 spawn 消除输出交错） |

完整 31 条记录与证据见 [`docs/evidence/README.md`](docs/evidence/README.md)。

一键自检：

```sh
make -C micro check
```

默认 QEMU 演示：

```sh
make -C micro run                   # BOOT_MODULES=capability + timer 抢占探针
make -C micro run BOOT_MODULES=all  # 20 个模块依次 PASS
make -C micro list-modules
make -C micro demo                  # 或 python3 tools/demo.sh（M7 交互验收）
```

## 模块一览

| 模块 | host `make test` | 单模块 QEMU | 真实 / 替身 | 教材（王道 / 汤小丹） |
| --- | --- | --- | --- | --- |
| objects | yes | `BOOT_MODULES=objects` | 真实（内核） | 2.1 / 2.2 |
| memory | yes | `memory` | 真实；demo 用 static 池，resident 用 BootInfo RAM | 3.1–3.2 / 第 6、7 章 |
| sched | yes | `sched` | 真实 | 2.2 / 第 2 章 |
| ipc | yes | `ipc` | 真实 | 2.3 / 3.4 |
| capability | — | `capability`（默认） | 真实 | 1.4、2.3 / 第 2、3 章 |
| clock | — | `clock` | 真实 | 1.5、5.2 / 9.2 |
| interrupt | yes | `interrupt` | 单模块为模型；resident 已接 PLIC MMIO | 1.5、5.2 / 9.2 |
| syscall | — | `syscall` | 真实双 AS/U-mode 闭环 | 1.5 / 1.5 |
| abi | — | `abi` | 真实 | 1.5 / 1.5 |
| init | — | `init` | 单模块为模型；resident 经 SM/PM 拉 fs/console/shell | 1.4、2.1 / 1.4、2.2 |
| libs | — | `libs` | 真实 U ecall | 1.5 / 1.5 |
| apps | — | `apps` | echo 模型；M7 shell 单独在 `userland/shell/` | 2.3 / 3.4 |
| shell | — | resident | 真实独立 U-mode AS；console 行输入 + FS/PM endpoint | M7 |
| pm | — | `pm` | 单模块 provider 替身；resident PM 维护真实进程表 | 2.1 / 第 2 章 |
| sm | — | `sm` | 单模块句柄替身；resident SM 真实 register/lookup + cap 派生 | 2.3 / 3.4 |
| vm | — | `vm` | 单模块为模型；resident U-mode VM + pager（M6） | 3.1–3.2 / 第 6、7 章 |
| vfs | — | `vfs` | resident 真实链路：FS 服务→VFS→ext4→virtio；单模块 demo 仍为内存模型 | 4.1–4.2 / 第 8 章 |
| block | — | `block` | resident 真实 virtio-blk；单模块 demo 仍为内存盘/合成完成 | 5.2–5.3 / 9.3–9.4 |
| ext4 | — | `ext4` | resident 真实镜像（含 depth-1/大文件）；单模块 demo 仍为内存镜像 | 4.1–4.3 / 第 8 章 |
| net | — | `net` | **链路替身**，非 M0–M7 主线 | 5.1 / 第 9 章 |
| nvme | — | `nvme` | **控制器替身**，非 M0–M7 主线 | 5.2–5.3 / 9.3–9.4 |

## 构建环境

`clang` + `ld.lld` 或 `rust-lld` + `qemu-system-riscv64`（virt，128M，OpenSBI default）；
M7 端到端（`make demo` / `make check`）还需要 `python3` 和 `mke2fs`（e2fsprogs）。

```sh
sudo apt install qemu-system-misc lld llvm clang e2fsprogs python3
```

## 证据索引

[`docs/evidence/README.md`](docs/evidence/README.md)

## 本树不是什么

- 不是完整 L4/seL4 产品（无 SMP、无 COW/swap、无完整 POSIX）
- 不是 `mono/` 的重复实现（禁止改 `mono/`）
- **`[xxx] PASS` 只证明该模块 README 写明的边界**，不自动等于「操作系统已做好」