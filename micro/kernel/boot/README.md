# kernel/boot

把 `arch` 交接过来的原始机器状态归一化成一份 `BootInfo`。完成早期初始化后，`kmain` 按镜像内的自举清单运行选定模块。

一句话模型：

```text
arch/riscv/boot.S  _start
        │  a0 = hartid, a1 = DTB
        ▼
kernel/boot/boot.c  boot_main(hartid, dtb)
        │  校验入口 → 早期控制台 → 构建/校验 BootInfo → 装 tp/stvec
        ▼
kmain(const struct bootinfo *)
        │  bootstrap_run()
        ▼
按清单顺序运行模块 → 退出 QEMU
```

QEMU 通过 `-kernel` 把 ELF 的可加载段写入 RAM。OpenSBI 随后交出控制权，由 `_start` 建栈、清零 `.bss` 并进入 `boot_main`。此时，包括自举清单在内的镜像内容已在内存中。 [QEMU RISC-V ELF 装载](https://github.com/qemu/qemu/blob/v8.2.2/hw/riscv/boot.c#L198-L220)

---

## 一、边界

| 做 | 不做 | 归属 |
|---|---|---|
| 把入口假设变成断言 | 从磁盘读内核 | `bootloader/` 或固件 |
| 建立早期控制台 | 实现正式 UART 驱动 | `userland/servers/drivers` |
| 校验 FDT 头并记录区间 | 完整解析 DTB 节点 | 未来 `kernel/platform/fdt.c` |
| 标记 RAM 与保留区 | 实现物理页分配器 | `kernel/memory` |
| 建立 `tp`、安装 `stvec` | 开中断、装 PLIC | `kernel/interrupt` |
| 交出 `BootInfo` 并调用 `kmain` | 创建首个线程/服务 | `kernel/objects` / `userland/init` |

判据只有一句：**如果 `kmain` 或 U-mode server 能在不违反入口不变量的前提下完成，它就不属于 `kernel/boot`。**

特别说明：本目录不是 BIOS/MBR/PBR 的实现层。RISC-V virt 的上游是 OpenSBI，`-kernel` 由 QEMU 的 ELF loader 负责，没有 MBR/PBR。x86 的完整引导链见 [`ex/x86-bios`](../../../ex/x86-bios/README.md)。

---

## 二、五个时刻

| # | 时刻 | 代码 | 只做什么 | 失败 |
|---|---|---|---|---|
| 0 | entry | `arch/riscv/boot.S` | 关 `sie/sip`、设 `sp`、清 `.bss`、保存 `a0/a1` | `wfi` |
| 1 | console | `boot.c` | 用 SBI legacy `putchar` 输出 | 静默 |
| 2 | parse/validate | `bootinfo.c` | 校验 FDT 头，构建并校验 `BootInfo` | fail-stop |
| 3 | reserve | `bootinfo.c` | 标记 firmware / kernel / DTB | fail-stop |
| 4 | arch + handoff | `boot.c` | `hart_set`、`trap_install`、`kmain(&bi)` | fail-stop / 不返回 |

第 0 步在 arch 里；boot 模块从 C 入口 `boot_main` 开始。这里列出它是为了标出责任的交界，而不是把汇编搬进 `kernel/boot`。

---

## 三、入口契约

`boot.S` 调用 `boot_main(u64 hartid, u64 dtb)` 时只保证：

| 项 | 值 | 谁保证 |
|---|---|---|
| 特权级 | S-mode | OpenSBI |
| `a0` | hartid | OpenSBI |
| `a1` | DTB 物理地址 | OpenSBI |
| `satp` | 0（bare） | OpenSBI |
| `sstatus.SIE` | 0 | OpenSBI |
| `sie` / `sip` | 0 | `boot.S` |
| `sscratch` | 0 | `boot.S` |
| `sp` | `__boot_stack_top` | `boot.S` |
| `.bss` | 全零 | `boot.S` |
| `stvec` | 未安装 | — |

`boot_main` 的第一件事是把这些假设变成断言，而不是默认它们成立：

- `.bss` 里放一个 `volatile` 探针，读到的必须是 0；
- `satp` 必须是 0；
- `sstatus.SIE` 必须是 0；
- `sie` 必须没有被提前使能；
- `sscratch` 必须是 0，否则 `trap.S` 的换栈判据不成立。

任一不成立就打印并关机。入口契约一旦被破坏，后面所有推断都不可信。

非 boot hart（`a0 != 0`）在 `_start` 的第一条分支就停车，不会碰共享启动栈和 `.bss`；
这让"单 hart 运行"从约定变成可执行事实。

---

## 四、BootInfo：唯一的启动契约

`BootInfo` 是 boot 与内核其余部分之间唯一的数据结构。它不描述设备、不描述策略，只描述**内存**和**镜像位置**。

### 4.1 为什么要有 magic / version / size

这是 Linux `boot_params` 的第一课：消费方必须能验证自己读到的结构是什么、有多大。没有这三件套，结构一改就是静默错位。

```c
struct bootinfo {
    u32 magic;          /* "BOOT" */
    u16 version;        /* 当前 1 */
    u16 size;           /* 当前 sizeof(struct bootinfo) = 448 B */
    u64 hartid;
    u64 boot_cpu;
    u64 dtb;
    u64 dtb_size;
    u32 ram_count;
    u32 reserved_count;
    struct boot_region ram[8];
    struct boot_region reserved[8];
    u64 kernel_start;
    u64 kernel_end;
};
```

### 4.2 区域类型

| 类型 | 含义 | 谁写 |
|---|---|---|
| `BOOT_REGION_RAM` | 可用的物理 RAM | `bootinfo_build` |
| `BOOT_REGION_FIRMWARE` | OpenSBI 占用，内核不得覆盖 | `bootinfo_build` |
| `BOOT_REGION_KERNEL` | 内核镜像 + `.bss` + 启动栈 | `bootinfo_build` |
| `BOOT_REGION_DTB` | 设备树 blob | `bootinfo_build` |
| `BOOT_REGION_INITRD` | 预留，当前未使用 | 下一步 |

区域用 `base + size`，不用起止指针；避免“end 指向最后一个字节还是之后”的歧义。

### 4.3 构建规则

`bootinfo_build` 只做加法：

1. `ram[0] = [RAM_BASE, RAM_BASE + RAM_SIZE)`；
2. `reserved += [RAM_BASE, KERNEL_BASE)`，即 OpenSBI；
3. `reserved += [__kernel_start, __kernel_end)`；
4. 如果 DTB 头有效，`reserved += [dtb, dtb + totalsize)`。

它不分配、不映射、不排序、不去重。去重和合并是内存管理模块的事。

### 4.4 校验规则

`bootinfo_validate` 检查的是**自洽性**，不是策略：

| 检查 | 失败码 |
|---|---|
| magic / version / size | `EBADMAGIC` / `EBADVERSION` / `EBADSIZE` |
| 区域非空、`base + size` 不回绕 | `EBADREGION` |
| RAM 之间不重叠 | `EOVERLAP` |
| 保留区之间不重叠 | `EOVERLAP` |
| 每个保留区都落在某段 RAM 内 | `ENOTINRAM` |
| kernel 区间非空、在 RAM 内、且有一条 KERNEL 保留区与之完全一致 | `EBADREGION` / `ENOTINRAM` |
| 若 `dtb_size != 0`，DTB 在 RAM 内且有对应保留区 | `EBADREGION` / `ENOTINRAM` |

校验失败时 `boot.c` 打印错误名并关机，不带病交接。

### 4.5 平台事实从哪来

`bootinfo.c` **不包含任何 CSR 或平台头文件**（只用到 `types.h` 的编号类型）。平台事实由 `boot.c` 的 `platform_fill` 填入：

| 字段 | 来源 |
|---|---|
| `ram_base` / `ram_size` | `arch/riscv/platform.h` |
| `firmware_base` / `firmware_size` | `RAM_BASE` 到 `KERNEL_BASE` |
| `kernel_start` / `kernel_end` | 链接脚本符号 `__kernel_start` / `__kernel_end` |

这样 `BootInfo` 的构建与校验可以脱离 RISC-V 单独测试；arch 相关常量只出现在 `boot.c` 这一层胶水里。

---

## 五、FDT 策略：只验头，不解析

当前只读 FDT 的 40 字节头：

```text
offset 0   magic       0xd00dfeed
offset 4   totalsize   big-endian
```

校验四件事：

1. 起始地址满足 FDT 规范的 8 字节对齐；
2. 头部本身在 RAM 内；
3. magic 正确；
4. `totalsize` 在 `[40, 1 MiB]` 内，且整个 blob 在 RAM 内。

三种结果：

| `a1` | 结果 | 行为 |
|---|---|---|
| 0 | `BOOT_DTB_NONE` | 使用 `platform.h` 的编译期常量，继续启动 |
| 非 0，头有效 | `BOOT_DTB_VALID` | 记录 `[dtb, dtb+size)` 为 DTB 保留区 |
| 非 0，头无效 | `BOOT_DTB_BAD` | fail-stop，不猜 |

**为什么现在不解析节点**：最小模型只需要内存和内核位置，`platform.h` 已给出 QEMU virt 的事实。完整解析 `/memory`、`/cpus/timebase-frequency`、`/chosen/stdout-path`、`/chosen/linux,initrd-*` 是下一步 `kernel/platform/fdt.c` 的事。届时 `platform.h` 从“事实来源”降级为“DTB 缺失时的显式回退”。

---

## 六、arch 代码在哪里被加载

`boot` 不是与 arch 平行的另一套实现；它在关键点上调用 arch：

| arch 符号 | 调用点 | 证明了什么 |
|---|---|---|
| `_start`（`boot.S`） | OpenSBI 把控制权交到 `0x80200000` | 入口地址与 `.text.entry` 布局正确 |
| `sbi_putchar`（`sbi.c`） | `con_puts` / `con_puthex` / `con_putdec` | S-mode `ecall` 能到 OpenSBI |
| `hart_set`（`hart.h`） | `boot_main` | `tp = &hart_local` 不变量建立 |
| `hart_local_get` | `boot_main` | 从 C 侧读回 `tp`，确认没写丢 |
| `trap_install`（`trap.S`） | `boot_main` | `stvec = trap_entry` |
| `csr_read`（`csr.h`） | 入口断言与 arch 证据 | 读回 `satp/sstatus/stvec`，不是“以为它是 0” |
| `sbi_shutdown`（`sbi.c`） | `boot_panic` / `kmain` | 失败路径和正常退出都能让 QEMU 结束 |

`context_switch` 保存启动执行流，完成一个模块后恢复它并推进清单。U-mode demo 通过 `trap_return` 进入用户态，首次 `ecall` 由 trap 打印诊断，模块结束由 `context_switch` 回到启动执行流。架构层还提供独立自检。 [arch 三时刻自检](../arch/riscv/README.md)

先执行 `hart_set`，再执行 `trap_install`，令陷阱入口安装后始终可通过 `tp` 取得 `hart_local`。

---

## 七、构建与运行

### 自举清单

从仓库根目录执行以下命令。

```sh
make -C micro list-modules
make -C micro run BOOT_MODULES='clock ipc init'
make -C micro run BOOT_MODULES='block,ext4'
make -C micro run BOOT_MODULES=all
make -C micro run BOOT_MODULES=
```

`BOOT_MODULES` 接受空格或逗号分隔的模块名。空清单只演示早期初始化，`all` 使用目录表的顺序。未知名称和重复名称在构建时报告错误。默认启动能力自检，镜像位于 `micro/build/riscv/kernel.elf`。其他清单按模块名和顺序保存在 `micro/build/riscv-bootstrap/<清单>/kernel.elf`。

构建时生成 `bootstrap_plan.c`，里面是按顺序排列的模块描述符指针。模块的入口函数、特权级和陷阱回调由各目录的 `DEMO_MODULE` 定义。镜像只链接所选模块及其依赖的实现。例如 `ipc` 会用到对象、内存和调度代码，清单仍只调用你点名的 demo。 [模块目录表](../../shared/modules.mk)

`BootInfo` 校验通过后，`bootstrap_run` 先校验清单中每个描述符（`name` / `run` 非空），再逐条执行。内核 demo 在 S-mode 运行，用户态 demo 进入 U-mode，完成后回到启动执行流。每项输出 `[模块名] start` 和 `[模块名] PASS`。全清单完成后输出 `[bootstrap] PASS` 并关机。模块断言失败会打印失败项并以非零状态退出。

默认镜像对应的 QEMU 命令如下。

```sh
qemu-system-riscv64 -machine virt -m 128M -smp 1 -nographic -monitor none \
  -bios default -kernel micro/build/riscv/kernel.elf
```

### 期望输出

OpenSBI 日志之后应出现：

```text
[micro] boot: minimal model
[boot] raw handoff: hartid=0 dtb=0x0000000087e00000
[boot] entry: bss=0, satp=0, SIE=0, sscratch=0
[boot] console: SBI legacy putchar OK
[boot] fdt: header OK
[boot] bootinfo: v1 size=448
[boot] kernel: 0x0000000080200000 + 0x0000000000050000
[boot] ram: 0x0000000080000000 + 0x0000000008000000
[boot] reserved[0]: 0x0000000080000000 + 0x0000000000200000 firmware
[boot] reserved[1]: 0x0000000080200000 + 0x0000000000050000 kernel
[boot] reserved[2]: 0x0000000087e00000 + 0x0000000000001516 dtb
[boot] arch: tp=&hart_local, stvec installed
[boot] arch: satp=0x0000000000000000 sstatus=0x8000000200006000
[boot] handoff: kmain(&bootinfo)
[kmain] handoff: BootInfo at 0x000000008024fca0, hartid=0
[bootstrap] begin
[capability] start
...
[cap] selftest ok
[capability] PASS
[bootstrap] PASS
[kmain] shutting down
```

具体数值会随内核大小和 QEMU 版本略变；关键是三段：`entry` 断言、`reserved`
列表、`arch` 证据。能力自检的完整输出见
[`kernel/capability/README.md`](../capability/README.md)。

### 验收表

| 输出 | 证明 |
|---|---|
| `hartid=0 dtb=0x87e00000` | `a0/a1` 穿过 `boot.S` 的清 BSS 到达 C |
| `bss=0, satp=0, SIE=0, sscratch=0` | 入口契约成立，不是假设 |
| `fdt: header OK` | DTB 对齐、魔数、长度、RAM 范围都过 |
| `bootinfo: v1 size=448` | 契约结构已被构建 |
| `kernel: 0x80200000 + ...` | 内核起止来自链接符号，并被 kernel 保留区完全覆盖 |
| `ram: 0x80000000 + 0x08000000` | RAM 来自平台事实，不是猜的 |
| `reserved[0]` | OpenSBI 的 2 MiB 被显式保护 |
| `reserved[1]` | 内核镜像 + BSS + 启动栈被显式保护 |
| `reserved[2]` | DTB 被显式保护 |
| `tp=&hart_local, stvec installed` | arch 的 `hart_set` 和 `trap_install` 已执行 |
| `kmain(&bootinfo)` | 早期初始化完成，开始读取本镜像的自举清单 |
| `[bootstrap] PASS` | 清单各项已按顺序结束 |

### QEMU 实测证据（启动闭环）

本次打通启动闭环的实测命令、退出码与完整串口记录如下（均在仓库根目录执行）：

| 场景 | 命令 | 退出码 | 完整输出 |
|---|---|---|---|
| 默认能力自检 | `make -C micro run` | 0 | [`qemu-boot-capability.log`](qemu-boot-capability.log) |
| U-mode ecall 路径 | `make -C micro run BOOT_MODULES=libs` | 0 | [`qemu-boot-umode.log`](qemu-boot-umode.log) |

两份 log 开头都记录实际执行的 `# command:` 和 `# exit code:`。OpenSBI 横幅带 NUL 字节，保存前用 `tr -d '\000'` 去掉，仅做纯文本化，未改动其他内容。

两条命令都真正执行 `qemu-system-riscv64 -machine virt -m 128M -smp 1 -nographic -monitor none -bios default -kernel ...`，不是宿主替身。内核在 `[bootstrap] PASS` 后调用 `sbi_shutdown`，QEMU 自行退出，因此不需要等满 15s 超时。

默认镜像的关键串口行为：

```text
[boot] entry: bss=0, satp=0, SIE=0, sscratch=0
[boot] console: SBI legacy putchar OK
[boot] fdt: header OK
[boot] bootinfo: v1 size=448
[boot] arch: tp=&hart_local, stvec installed
[boot] handoff: kmain(&bootinfo)
[bootstrap] begin
[capability] start
...
[capability] PASS
[bootstrap] PASS
[kmain] shutting down
```

U-mode 模块（`libs`）的源码路径是 `userland/libs/demo.c` 的 `DEMO_MODULE(libs, true, run, handle_trap)`（`user=true`）。启动后由 `shared/demo.c` 的 `demo_enter_user` 构造 U-mode 帧并 `sret` 进入用户态；用户代码的 `DEMO_PRINT`、IPC 与 `ABI_THREAD_EXIT` 都经 `ecall` 陷回内核。首次 U-mode `ecall` 由内核打印 `scause`/`SPP`/`sepc`，`[libs] PASS` 是内核在收到用户 `ecall` 后打印的响应，不是宿主伪造。`ABI_THREAD_EXIT` 经 `demo_pass` → `context_switch` 回到 boot 执行流，因此这份日志描述的是「进入 U-mode → ecall 陷回内核 → 上下文切回 boot」，**不**单独证明从 U-mode 用 `sret` 返回整个模块，也不提供 U-mode 寄存器逐项恢复的独立证据：

```text
[bootstrap] begin
[libs] start
[umode] first ecall: scause=0x0000000000000008 SPP=0 sepc=0x000000008020216a
runtime: stack alignment, IPC wrappers, signed error and thread exit
[libs] PASS
[bootstrap] PASS
[kmain] shutting down
```

`libs` 的用户串口输出经 `DEMO_PRINT` ecall 由内核代打。完整证据以两份 log 文件为准。

正式启动镜像不主动触发 S-mode 非法指令；同步异常原因与 `sepc` 的实测证据来自独立架构自检，`trap.c` 的非法指令恢复分支只在自检 Makefile 定义的 `ARCH_SELFTEST` 下编译，正式路径遇到非法指令会 `panic`。见 [`arch/riscv/README.md`](../arch/riscv/README.md) 第八节与 [`qemu-selftest.log`](../arch/riscv/qemu-selftest.log)。

---

## 八、显式不做与已知限制

| 限制 | 现状 | 归属 |
|---|---|---|
| 单 hart | 只用 hart 0；其他 hart 在 `_start` 直接 `wfi` 停车 | 后续 SMP 切面 |
| DTB 节点解析 | 只验头，用 `platform.h` 常量 | `kernel/platform/fdt.c` |
| 自举模块来源 | 构建时链接进 ELF。独立的初始用户映像可用 initrd 描述 | `bootstrap_plan.c` |
| 物理页分配器 | 未实现，只给区间 | `kernel/memory` |
| 首个线程/服务 | 未创建 | `kernel/objects` / `userland/init` |
| `kmain` | 完成选定的 demo 后关机 | `kernel/boot/bootstrap.c` |
| context/trap | 每次自举使用共享切换和陷阱入口，架构自检有独立镜像 | `arch/riscv/README.md` §八 |
| MBR/PBR | RISC-V virt 不存在这条链 | `ex/x86-bios` |

`BootInfo` 分配在启动栈上。各 demo 运行期间，启动栈由 `context_switch` 保存，清单结束后恢复。

---

## 九、下一步

1. **`kernel/platform/fdt.c`**：解析 `/memory`、`timebase-frequency`、`stdout-path`、`initrd`，让 `BootInfo` 不再依赖 `platform.h`。
2. **初始用户态 ELF**。在启动契约中记录独立用户镜像的物理区间，由内核据此建立用户地址空间。
3. **SMP**：hart lottery、per-hart `hart_local` 数组、非 boot hart 的 `wfi` 停车场。
4. **真正的 `kmain`**：接 `kernel/objects` 创建首个地址空间/线程，或接 `userland/init` 完成第一次 IPC。
