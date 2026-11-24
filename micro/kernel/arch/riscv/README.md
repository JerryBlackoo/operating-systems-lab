# kernel/arch/riscv

把 RISC-V 机器状态翻译成内核抽象的唯一一层。

本目录**不实现任何内核策略**：不决定哪个线程跑、不管页从哪来、不解析路径。它只回答三个问题：

| 时刻 | 问题 | 文件 |
|---|---|---|
| 1 | CPU 上电后，怎么让 C 代码跑起来 | `boot.S` |
| 2 | 用户/硬件敲内核门，怎么进去、怎么回来 | `trap.S` + `trap.c` |
| 3 | 内核想换线程跑，怎么保存、怎么恢复 | `context.S` |

其余文件（`csr.h`、`hart.h`、`sbi.c`、`platform.h`、`link.ld`）都是这三个时刻的**支撑材料**，不含独立模型。

**运行三个时刻的独立自检**：在本目录执行 `make run`；命令、调用链、输出与验证范围见[第八节](#八独立自检运行与验收输出)。`micro/` 目录下的 `make run` 是另一条正式 boot 路径。

---

## 一、寄存器分类（本模块的作用域）

RISC-V 的一台 hart 在任意瞬间，可被本模块触碰的状态只有四类：

| 类别 | 数量 | 本模块的角色 |
|---|---|---|
| **GPR**（x0–x31） | 32 | 时刻 2 全部保存；时刻 3 只保存 14 个 |
| **CSR** | 本模块用 8 个 | 时刻 2 存 4 个；其余按需读写 |
| **内存** | — | `.bss` 清零、trap frame、`hart_local` |
| **MMIO** | 1 处 | `sbi.c` 的关机退路 |

**`x0` 从不保存**：恒为 0，存了浪费 8 字节。

### 1.1 GPR 的三类分组（决定存不存）

| 组 | 寄存器 | 时刻 2（trap.frame） | 时刻 3（context） | 理由 |
|---|---|---|---|---|
| caller-saved | `ra, t0–t6, a0–a7` | **存** | 只存 `ra` | 用户可见，必须原样还；内核侧编译器已保证无需保留 |
| callee-saved | `s0–s11, sp` | **存** | **存** | 跨调用必须保留 |
| 特殊 | `gp` | **存** | 不存 | 用户可能改；内核内 gp 恒定 |
| 特殊 | `tp` | **存但恢复时按帧值写回** | 不存 | 见 §6 不变量 I1 |
| 零 | `x0` | 不存 | 不存 | 恒 0 |

### 1.2 本模块涉及的 8 个 CSR

| CSR | 编号 | 读 | 写 | 出现的文件 |
|---|---|---|---|---|
| `sstatus` | 0x100 | ✅ | ✅ | `csr.h`（irq_*）、`trap.S` |
| `sie` | 0x104 | — | ✅（只清零） | `boot.S` |
| `stvec` | 0x105 | ✅ | ✅ | `trap.S`（`trap_install` 写）；`boot.c` / `selftest.c` 读回 |
| `sscratch` | 0x140 | ✅ | ✅ | `boot.S`、`trap.S` |
| `sepc` | 0x141 | ✅ | ✅ | `trap.S` |
| `scause` | 0x142 | ✅ | — | `trap.S` |
| `stval` | 0x143 | ✅ | — | `trap.S` |
| `sip` | 0x144 | — | ✅（只清零） | `boot.S` |

**从未被触碰**（显式边界，见 §5）：`satp`、`sbi` 相关 M-mode CSR、`mhartid`、`time`（除自检故意写）。

---

## 二、逐文件：对寄存器做了什么

### `boot.S` — 时刻 1

| 操作 | 目标 | 值 | 为什么 |
|---|---|---|---|
| `csrw` | `sie` | 0 | OpenSBI 只保证 `sstatus.SIE=0`，`sie` 位可能是脏的 |
| `csrw` | `sip` | 0 | 同上 |
| `csrw` | `sscratch` | 0 | **建立不变量 I1 的 S-mode 分支**；`trap.S` 靠零值判来源 |
| `la` | `sp` | `__boot_stack_top` | C 函数第一行就可能用栈 |
| `mv` | `s0` ← `a0` | hartid | 清 bss 会用 `t0/t1`，先搬走 |
| `mv` | `s1` ← `a1` | DTB 物理地址 | 同上 |
| `la` | `t0` ← `__bss_start` | | 循环指针 |
| `la` | `t1` ← `__bss_end` | | 循环边界 |
| `sd` | `[t0]` ← `x0` | 0 | 清 `.bss`，每 8 字节 |
| `addi` | `t0` += 8 | | |
| `mv` | `a0` ← `s0` | hartid | 恢复参数 |
| `mv` | `a1` ← `s1` | dtb | |
| `call` | `ra` ← 返回地址 | | 进 `boot_main` |
| `wfi` | — | | `boot_main` 返回即设计错误 |

**不碰**：`tp`、`sstatus`（SIE 保持 OpenSBI 留的 0）、`satp`、`stvec`、`sepc/scause/stval`。

**副作用**：`ra` 被 `call` 覆盖（可接受，尚无返回语义）。

---

### `trap.S` — 时刻 2

#### 入口：`trap_entry`

| 指令 | 目标寄存器 | 效果 |
|---|---|---|
| `csrrw sp, sscratch, sp` | `sp` ↔ `sscratch` | **一条指令完成换栈**：U 陷入时 `sp`←内核栈顶、`sscratch`←用户 sp；S 陷入时 `sp`←0 |
| `bnez sp, .Lfrom_u` | — | 用 `sp==0` 判定「来自 S-mode」（依赖 I1） |

**S-mode 路径**（`.Lfrom_s`）：

| 指令 | 目标 | 效果 |
|---|---|---|
| `csrr sp, sscratch` | `sp` | 取回原内核 sp |
| `csrw sscratch, zero` | `sscratch` | **恢复零值不变量** |
| `addi sp, sp, -288` | `sp` | 分配帧 |
| `sd t0, 16(sp)` | 内存 | 先存原始 `t0`（马上要用它） |
| `addi t0, sp, 288` | `t0` | 原始 sp = 帧基址 + 288 |
| `sd t0, 232(sp)` | 内存 | 存入帧的 `sp` 槽 |

**U-mode 路径**（`.Lfrom_u`）：

| 指令 | 目标 | 效果 |
|---|---|---|
| `addi sp, sp, -288` | `sp` | 分配帧（此时 sp 已是内核栈） |
| `sd t0, 16(sp)` | 内存 | 先存用户 `t0` |
| `csrr t0, sscratch` | `t0` | 取出用户 `sp` |
| `sd t0, 232(sp)` | 内存 | 存入帧的 `sp` 槽 |
| `csrw sscratch, zero` | `sscratch` | 内核态期间归零 |

**汇合点 `.Lsave_rest`**：`sd` 保存 **29 个 GPR** 到固定偏移（`ra, gp, t1–t6, s0–s11, a0–a7, tp`），然后：

| 指令 | 读取 CSR | 写入帧偏移 |
|---|---|---|
| `csrr t0, sepc` → `sd, 248` | `sepc` | 248 |
| `csrr t0, sstatus` → `sd, 256` | `sstatus` | 256 |
| `csrr t0, scause` → `sd, 264` | `scause` | 264 |
| `csrr t0, stval` → `sd, 272` | `stval` | 272 |

#### 出口：`trap_return`

| 指令 | 目标 | 效果 |
|---|---|---|
| `mv sp, a0` | `sp` | **切帧**：`a0` 可以是入参，也可以是另一个线程的 frame（I6） |
| `ld t0, 256(sp)` + `andi t0, t0, 0x100` | `t0` | 取 `sstatus.SPP` 判目标特权级 |
| `addi t0, sp, 288` + `csrw sscratch, t0` | `sscratch` | 回 U：内核栈顶 = 帧尾 |
| `csrw sscratch, zero` | `sscratch` | 回 S：归零 |
| `ld t0, 256(sp)` + `csrw sstatus, t0` | `sstatus` | **必须早于 sret**（sret 从 SPIE 恢复 SIE） |
| `ld t0, 248(sp)` + `csrw sepc, t0` | `sepc` | 返回地址 |
| `ld` × 29 | GPR | 恢复；**`t0`/`tp`/`sp` 必须最后三个** |
| `sret` | `pc, SIE, 特权级` | 硬件：`pc←sepc`、`SIE←SPIE`、模式←SPP |

**不碰**：`sie`、`sip`、`stvec`、`satp`。

**顺序约束**（写在代码注释里，改序必须重验）：
1. `sstatus` / `sepc` 必须在恢复 GPR **之前**写（恢复会覆盖 `t0`）。
2. `sp` 必须**最后**恢复（此后再无帧可访问）。

---

### `context.S` — 时刻 3

**零 CSR 操作。** 只读写 GPR 和内存。

| 指令 | 目标 | 偏移 |
|---|---|---|
| `sd ra, 0(a0)` | 保存 A 的 `ra` | 0 |
| `sd sp, 8(a0)` | `sp` | 8 |
| `sd s0..s11, 16..104(a0)` | 12 个 callee-saved | 16–104 |
| `ld ra, 0(a1)` … `ld s11, 104(a1)` | 恢复 B | 同上 |
| `ret` | `pc ← ra` | 跳到 **B 上次停的地方** |

共 **14 存 + 14 读**。

**为什么不存 31 个**：这是内核自己发起的函数调用，编译器保证 caller-saved（`a0–a7, t0–t6`）在调用后无意义；需要跨调用保留的值已被压进栈，而栈由 `sp` 代表。`sp + callee-saved` 足以恢复任意内核执行流。

**`ret` 的语义**：不是返回 `context_switch` 的调用者，而是返回 `*to*` 上次保存时的 `ra`。

---

### `csr.h` — 唯一允许拼 CSR 指令的地方

**六个原语**（都是 `static inline` / 语句表达式，零函数调用开销）：

| 宏 | 指令 | rd=x0 时 | rs1=x0 时 |
|---|---|---|---|
| `csr_read(csr)` | `csrr rd, csr` | — | — |
| `csr_write(csr, v)` | `csrw csr, rs` | — | 写 0 |
| `csr_set(csr, m)` | `csrs csr, rs` | — | **不写** |
| `csr_clear(csr, m)` | `csrc csr, rs` | — | **不写** |
| `csr_read_clear(csr, m)` | `csrrc rd, csr, rs` | — | 不写 |
| `csr_read_set(csr, m)` | `csrrs rd, csr, rs` | — | 不写 |

全部带 `"memory"` clobber（与 Linux 6.14 `arch/riscv/include/asm/csr.h` 一致）：部分 CSR 读写有顺序要求，编译器无法区分，代价可忽略。

**中断屏蔽（只碰 `sstatus`）**：

| 函数 | 实现 | 说明 |
|---|---|---|
| `irq_save()` | `csrrc sstatus, SIE` | **必须单指令**；两句写法会丢状态 |
| `irq_disable()` | `csrc sstatus, SIE` | |
| `irq_enable()` | `csrs sstatus, SIE` | |
| `irq_restore(f)` | `f & SIE ? csrs : nop` | 支持嵌套 |
| `irq_enabled()` | `csrr sstatus` & SIE | |

**单 hart 公理**：`irq_save/restore` 即全局互斥锁。引入 SMP 后失效。

---

### `hart.h` — 只碰 `tp`

| 函数 | 指令 | 说明 |
|---|---|---|
| `hart_set(h)` | `mv tp, h` | 建立 I1 |
| `hart_local_get()` | `mv %0, tp` | 读回 |

这两个是 `tp` 在全模块的**唯一出入口**。

---

### `sbi.c` — 只碰参数寄存器 + 一处 MMIO

| 函数 | 写入的 GPR | `a7`(EID) | 备注 |
|---|---|---|---|
| `sbi_call(...)` | `a0, a1, a2, a3, a6, a7` | 由调用者定 | `a6`=FID；`ecall` 后 `a0/a1` 带回返回值 |
| `sbi_putchar(ch)` | `a0`=ch | `0x01` | legacy |
| `sbi_set_timer(t)` | `a0`=t | `0x00` | legacy，**一次性**，不会自动重装 |
| `sbi_shutdown()` | `a0,a1`=0 | `0x53525354` | SRST；失败则写 MMIO |

**`sbi_shutdown` 的退路**：`*(volatile u32 *)0x00100000 = 0x5555`（QEMU virt 的 `sifive_test`）。

**关键事实**：`ecall` 从 S-mode 产生 `scause=9`，**被 M-mode 的 OpenSBI 截获**（`medeleg[9]=0`），永远不进 `trap_handler`。若进了，说明固件没加载 → `panic`。

---

### 陷阱入口和 C 分发

| 函数 | 寄存器/Memory 操作 |
|---|---|
| `trap_install()`（`trap.S`） | `csrw stvec, &trap_entry`（Direct 模式，mode=0） |
| `trap_handler(tf)` | 读 `tf->scause`、`tf->sstatus`；`ecall` 时 `tf->sepc += 4`；仅编译时定义 `ARCH_SELFTEST` 的自检镜像在 S-mode 非法指令时 `+=4`（假设故障指令为 4 字节非压缩编码），正式路径直接 `panic` |
| `panic()` | 读 `tf` 四个字段 + `hart_local_get()->hartid`（经 `tp`） |

**分发依据**：`scause` 最高位 63 = 中断标志；`sstatus.SPP` 判来源。

---

### `platform.h` / `link.ld` / `types.h` / `trap.h` / `context.h`

**零寄存器操作。**

| 文件 | 内容 |
|---|---|
| `platform.h` | QEMU virt 常量（RAM/CLINT/PLIC/UART/关机设备/SBI EID） |
| `link.ld` | 地址布局：`0x80200000`、`.text.entry` 最前、`.bss` NOLOAD、导出 `__bss_*` / `__boot_stack_*` / `__kernel_end` |
| `types.h` | `u8..u64`、`pte_t`、`PAGE_SIZE` |
| `trap.h` | `struct trap_frame`（288 B）+ 偏移宏 + 4 个函数声明 |
| `context.h` | `struct context`（112 B） |

---

## 三、汇总矩阵：谁独占哪个寄存器

### 3.1 GPR

| 寄存器 | boot.S | trap.S | context.S | sbi.c | hart.h |
|---|---|---|---|---|---|
| `x0` | 用作零源 | 不用 | 不用 | 不用 | 不用 |
| `ra` | `call` 写 | 存/恢复 | **存/恢复** | `ecall` 写 | — |
| `sp` | 初始化 | **交换/切帧** | **存/恢复** | — | — |
| `gp` | — | 存/恢复 | 不存 | — | — |
| `tp` | **不碰** | 存/恢复 | 不存 | — | **唯一出入口** |
| `t0–t6` | `t0/t1` 用作循环 | 存/恢复 | 不存 | — | — |
| `s0–s11` | `s0/s1` 暂存参数 | 存/恢复 | **存/恢复** | — | — |
| `a0–a7` | 参数传递 | 存/恢复 | 不存 | **参数/返回值** | — |

### 3.2 CSR（写权限唯一归属）

| CSR | 唯一写者 | 唯一读者 | 说明 |
|---|---|---|---|
| `sie` | `boot.S`（只清零） | — | 尚未使能任何中断 |
| `sip` | `boot.S`（只清零） | — | |
| `sscratch` | `boot.S` / `trap.S` | `trap.S` | 双态协议 |
| `stvec` | `trap.S`（`trap_install`） | `boot.c` / `selftest.c` | 只装一次 |
| `sepc` | `trap.S`（`trap_return`） | `trap.S` | 硬件在陷入时也写 |
| `scause` | **仅硬件** | `trap.S` | 软件不写 |
| `stval` | **仅硬件** | `trap.S` | 软件不写 |
| `sstatus` | `trap.S`（`trap_return`）+ `csr.h`（仅 SIE 位） | `csr.h` / `trap.S` | 分位所有权 |
| `satp` | **无人** | **无人** | 尚未开 MMU |

---

## 四、硬件隐式行为（不是我们写的，但定义了我们的契约）

| 时刻 | 硬件自动做的事 |
|---|---|
| **陷入时** | `sepc ← 出错/陷入指令 PC`；`scause ← 原因`；`stval ← 地址/指令`；`sstatus.SPP ← 原特权级`；`sstatus.SPIE ← 原 SIE`；`sstatus.SIE ← 0`；`pc ← stvec` |
| **`sret` 时** | `pc ← sepc`；`SIE ← SPIE`；特权级 ← `SPP` |
| **`ecall` 从 S-mode** | `scause=9`，被 M-mode 截获（`medeleg[9]=0`），**不进本模块** |
| **`ecall` 从 U-mode** | `scause=8`，进 `trap_entry`（`medeleg[8]=1`） |

OpenSBI 交接时的实际值（来自 QEMU 输出）：

```text
Boot HART MEDELEG : 0x0000000000f0b509   ← bit8=1(U ecall 给 S), bit9=0(S ecall 留 M)
Boot HART MIDELEG : 0x0000000000001666   ← bit5=1(定时器给 S), bit9=1(外部给 S)
Boot HART MISA    : rv64imafdch          ← 有 F/D，但编译期禁用
```

---

## 五、显式不做（边界声明）

这些**不是遗漏**，是划给别的模块：

| 没做的事 | 归属 | 判据 |
|---|---|---|
| `satp` 写入、`sfence.vma`、PTE 构造 | `kernel/memory` | 页从哪来是策略；arch 只该提供 PTE 位布局 |
| 页表遍历 / 映射 / 解除映射 | `kernel/memory` | 同上 |
| `sie` 使能中断（STIE/SEIE） | `kernel/interrupt` | arch 只提供开关原语 |
| PLIC claim/complete | 未来 `plic.c` | 属机器服务，尚未纳入最小模型 |
| 定时器 deadline 管理（重装、比较、时间片） | `kernel/clock` | arch 只该提供 `rdtime` + `sbi_set_timer` |
| 线程选择 | `kernel/sched` | arch 只提供 `context_switch` |
| 正式 UART 驱动 | `userland/servers/drivers` | `sbi_putchar` 只是早期输出 |
| DTB 解析 | 未来 `platform` 模块 | 最小模型用编译期常量；`a1` 已保存备用 |
| FPU / Vector 上下文 | 后续切面 | 编译期 `-march=rv64imac...`，保证 `sstatus.FS=OFF` |
| 多 hart 启动 | 后续切面 | 当前单 hart；`hart_local` 结构已留扩展位 |

---

## 六、不变量（代码必须维持）

| # | 不变量 |
|---|---|
| **I1** | 内核态（S-mode）`tp` 恒为 `&hart_local[hartid]`；当且仅当 U-mode 时 `sscratch` = 该线程内核栈顶，S-mode 时 `sscratch == 0`。U-mode 的 `tp` 属于用户上下文，本模型约定用户不得改写它 |
| **I2** | trap frame 是跨特权级传递寄存器状态的**唯一**结构（288 B，31 GPR + 4 CSR + pad） |
| **I3** | 进入 `trap_handler` 时 `sstatus.SIE = 0`（硬件保证） |
| **I4** | 任何 `satp` 写入后必须 `sfence.vma`（**尚未实现**，MMU 未开） |
| **I5** | 内核代码/数据/栈/trap frame 在所有地址空间中地址相同且 `PTE_U=0`（尚未开 MMU，待验证） |
| **I6** | `trap_handler` 返回值 = 下一个要跑的 frame，**不一定等于入参** |
| **I7** | 内核不可重入 trap（trap 期间关中断）；内核栈溢出为未定义行为 |

**I6 是与 Linux 的结构性差异**：Linux 的 `ret_from_exception` 隐含「回到 current」，微内核必须允许「这个线程不跑了」。

---

## 七、文件清单

```
kernel/arch/riscv/
├── boot.S          时刻 1：_start 五步
├── trap.S          时刻 2：trap_entry + trap_return
├── context.S       时刻 3：14 字上下文切换
├── trap.c          唯一 trap 分发 + panic（非法指令恢复仅在 ARCH_SELFTEST）
├── csr.h           6 个 CSR 原语 + irq_* + 常量
├── hart.h          hart_local + tp 出入口
├── trap.h          struct trap_frame + 偏移宏
├── context.h       struct context
├── platform.h      QEMU virt 常量
├── sbi.c / sbi.h   ecall 封装（legacy）
├── types.h         最小类型
├── link.ld         地址布局
├── selftest.c      独立三时刻自检的 boot_main
├── Makefile        自检镜像的构建与运行入口
└── SELFTEST.md     自检运行说明
```

配套（不在本目录，但属最小可运行集）：

| 文件 | 作用 |
|---|---|
| `micro/Makefile` | clang + ld.lld/rust-lld，`make run` |
| `micro/kernel/boot/` | **正式的 boot 模块**：`boot_main` → `BootInfo` → `kmain`，见 [`kernel/boot/README.md`](../../boot/README.md) |

正式 boot/demo 镜像**不链接 `trap.c`**：`shared/demo.c` 提供了同签名的
`trap_handler`，负责 demo 的 ecall 协议。两者都必须遵守 `trap.S` 的 ABI
（入参/返回值都是 `struct trap_frame *`），并由 `trap.S` 统一恢复现场。

---

## 八、独立自检：运行与验收输出

旧的临时 harness 已不在当前工作区；`selftest.c` 根据历史记录**重新实现**了三个时刻的自检，并不是找回的原文件。它复用本目录现有的启动、context 和 trap 代码，不修改正式 boot 模块。

### 8.1 运行

从仓库根目录运行：

```sh
make -C micro/kernel/arch/riscv run
```

也可以先进入本目录再执行 `make run`：

```sh
cd micro/kernel/arch/riscv
make run
```

需要 `clang`、`ld.lld` 或 Rust 工具链的 `rust-lld`，以及 `qemu-system-riscv64`。自检 Makefile 为本镜像定义 `-DARCH_SELFTEST=1`，因此只有这条自检路径会跳过 S-mode 非法指令并从 `sepc + 4` 继续；正式内核镜像不定义该宏，遇到非法指令会 `panic`。构建产物放在 `micro/build/riscv-arch-selftest/`；本目录执行 `make clean` 只清理该自检目录。QEMU 正常关机时退出码为 0；自检断言失败会打印 `[selftest] FAIL` 并请求 QEMU 非零退出。

在 `micro/` 运行 `make run` 会启动 boot 镜像并执行 `BOOT_MODULES` 清单。这里的非法指令和寄存器自检使用独立镜像。 [自举清单](../../boot/README.md#自举清单)

### 8.2 调用链

```text
QEMU virt → OpenSBI → boot.S:_start → selftest.c:boot_main
                                         ├─ hart_set / hart_local_get
                                         ├─ trap.S:trap_install → stvec = trap_entry
                                         ├─ context.S:context_switch(A → B → A)
                                         ├─ 写只读 CSR time → CPU 产生非法指令异常
                                         │   → trap.S:trap_entry → trap.c:trap_handler
                                         │   → trap.S:trap_return → sret → 继续执行
                                         └─ sbi.c:sbi_shutdown → QEMU 退出
```

独立镜像里的 `boot_main` 来自 `selftest.c`；正式启动镜像使用 `kernel/boot/boot.c` 的 `boot_main`。两者分别链接，故不冲突。执行的是 QEMU 模拟的 RISC-V 指令，并非物理 RISC-V CPU。

### 8.3 输出

前半段是 OpenSBI 的横幅和平台信息；其后是内核自检输出，例如（DTB 地址随运行环境变化）：

```text
[micro] arch/riscv minimal model
[boot] hartid=0  dtb=0x0000000087e00000
[boot] tp = &hart_local  OK
[boot] bss cleared (switch_count=0)  OK
[trap] stvec installed  OK
[ctx] switching A -> B
[ctx] in B, switching back to A
[ctx] back in A, switch_count=1  OK
[trap] writing read-only CSR (time)...
[trap] illegal instruction caught in S-mode: scause=0x0000000000000002 sepc=0x0000000080200964 stval=0x00000000c0101073, resuming
[trap] scause=0x0000000000000002 (illegal instruction) sepc=0x0000000080200968 expected=0x0000000080200968 stval=0x00000000c0101073
[trap] survived, trap return works  OK

[micro] all three moments verified, shutting down
```

上例是本机 QEMU 8.2.2 + OpenSBI v1.3 的实测输出，`sepc` 的绝对地址随镜像布局变化。完整串口记录（含 OpenSBI 横幅）见 [`qemu-selftest.log`](qemu-selftest.log)；文件开头记录实际命令和退出码，保存前去掉 OpenSBI 横幅中的 NUL 字节，仅为纯文本化，未改动其他内容。

| 输出 | 证明了什么 |
|---|---|
| `hartid=0  dtb=0x...` | 输出传入 C 的参数，可与 OpenSBI 的 hart ID、Next Arg1 人工对照；程序未自动比较 |
| `tp = &hart_local` | I1 在 C 侧可读回 |
| `bss cleared` | `.bss` 探针 `switch_count` 的初值为 0 |
| `stvec installed` | 读回的 `stvec` 等于 `trap_entry`，此时尚未触发 trap |
| `switch_count=1` | 执行 B 后成功切回 A，计数为 1；不等于逐一验证全部寄存器取值 |
| `illegal instruction caught ... scause=0x2 sepc=0x... stval=0x...` | trap 路径本身打印同步异常的原因码、出错 PC 和 `stval`；`sepc` 指向触发 `csrw time` 的那条指令 |
| `scause=... (illegal instruction) sepc=... expected=...` | 返回后读回 CSR：`scause=2`，`sepc` 已按 `+4` 前进且等于期望值，证明保存→分发→`sret` 全链路正确 |
| `survived` | 非法指令后没有停机，继续执行到正常关机 |

**本自检未验证**：U-mode 陷入与返回、`IRQ_S_TIMER`、MMU、多 hart、`trap_handler` 返回另一个线程的 frame。它只走 S-mode trap 路径，不代表用户态路径已验证。U-mode 的首次 `ecall` 由正式 boot 镜像的 `libs` 模块留下 `scause`/`SPP`/`sepc` 诊断，见 [`boot/README.md`](../../boot/README.md) 的“QEMU 实测证据”一节。`trap.c` 对 S-mode 非法指令加 4 并继续运行**只**在自检 Makefile 定义 `ARCH_SELFTEST` 时编译进来，属于自检用途；正式内核路径不会静默跳过非法指令。
