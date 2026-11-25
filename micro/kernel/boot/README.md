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

## 本目录的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct bootinfo` | `bootinfo.h` | **唯一的启动契约**：RAM 区间、kernel start/end、DTB 位置、initrd 区间、保留区数组、UART/PLIC 基址 |
| `struct boot_region` | `bootinfo.h` | 一个物理保留区（`base` + `size` + `type`），如 initrd、DTB |
| `struct boot_platform` | `bootinfo.h` | 平台事实：`ram_base`/`ram_size`、`firmware_*`、`kernel_start`/`kernel_end`、设备基址 |
| `struct demo_module` | `shared/demo.h` | 一个可运行模块：名字 + `run()` + 可选 `trap()` |
| `bootstrap_plan[]` | 构建生成 | 按 `BOOT_MODULES` 生成的模块指针数组，以 NULL 结尾 |
| `kmain()` | `kernel_init.c` | capability 镜像的真正主函数：帧池、内核 AS、UART/PLIC、storage bringup、服务装配、启动调度 |
| `struct service_boot_layout` | `service_boot.h` | 六个服务的启动寄存器布局（init/sm/pm/vm/heaptest/heaptest2） |
| `struct kernel_spawn_*` | `kernel_spawn.c` | PM spawn 的实现（`ABI_PM_SPAWN` / `ABI_VM_GRANT_AS` / `ABI_PM_DISCARD`） |
| `resident_user_trap()` | `resident_trap.c` | U-mode `ecall` 的常驻入口：分发、park、延迟终止兑现 |

## 与其他模块的关联

```text
arch/riscv/boot.S ──(hartid, dtb)──> boot.c ──> bootinfo ──> kmain
                                                              │
        ┌─────────────────────────────────────────────────────┤
        │            │            │          │                │
     memory       sched        capability   loader          storage
   frame_init   sched_init    cap_system_init spawn_user   bringup
        │            │            │          │                │
        └────────────┴────────────┴──────────┴────────────────┘
                                 │
              service_boot（cap 装配）+ kernel_spawn（PM spawn）
                                 │
                        resident_trap（syscall 入口）
```

| 对方模块 | 关系 |
| --- | --- |
| `arch/riscv` | `boot.S` 交出 `(hartid, dtb)`；`trap.S` 提供 `trap_entry`/`trap_return` |
| `memory` | `frame_init`（BootInfo RAM）+ `frame_reserve`（内核镜像与 initrd）+ `as_map_kernel_range` 建内核 AS |
| `sched` | `sched_init` / `thread_create`（idle）+ `sched_set_idle` + `sched_start_tick` |
| `capability` | `cap_system_init` + `cap_root_install` 装配每个服务的初始 CSpace |
| `loader` | `loader_pin_initrd` + `loader_spawn_user` 拉起六个服务 |
| `console` / `interrupt` / `arch/riscv/plic` | `console_mmio_init` + `plic_hw_init` + `irq_init` + `console_bind_uart_irq` |
| `storage` | `kernel_storage_bringup`（virtio-blk + block 队列） |
| `pager` | `pager_init(ep_vm)` |
| `syscall` / `user_backend` | `resident_trap` 调 `syscall_dispatch`；`user_syscall_backend_reset` 清后端 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 裸 `kmain` | 直接读 DTB、自己算 RAM 布局、手写每个模块初始化 | 早期能跑，但每加一个模块都要改主函数 |
| 2. `BootInfo` 契约 | 把 arch 交接的机器状态归一化成一份结构体；`boot_main` 负责构建与校验 | arch 与后续模块解耦；换板子只改 boot 层 |
| 3. 自举清单 | `BOOT_MODULES` → 构建期生成 `bootstrap_plan[]`；`bootstrap_run()` 按序执行 | 单模块 demo 与整内核镜像共用一套代码，`make check` 可逐模块验证 |
| 4. 五时刻模型 | 校验入口 → 早期控制台 → 构建 BootInfo → 装 `tp`/`stvec` → 进 `kmain` | 每个阶段的可用设施不同（SBI vs MMIO、有无 trap），必须严格排序 |
| 5. FDT 只验头 | 不解析 DTB 树，只校验魔数与边界；RAM/设备地址写成常量 | 教学模型不需要通用 FDT 解析器；代价是换板子要改常量 |
| 6. 常驻 `kmain` | `kernel_init.c` 取代 demo 驱动的 `bootstrap_run` | M7 需要一个真正「跑起来不退出」的镜像，而不是跑完 demo 就关机 |
| 7. 服务装配分离 | `service_boot_prepare` 负责 cap 布局，`kernel_spawn` 负责 PM spawn | 启动期和运行期的 cap 装配逻辑不同，混在一起会互相干扰 |
| 8. `resident_trap` 常驻入口 | U-mode `ecall` 统一进 `syscall_dispatch`；`WAITING` 时 `syscall_wait_park` | 把 syscall 分发从 demo harness 移到生产路径 |
| 9. storage 迁出策略层（A1） | `kernel_storage_bringup` 只保留 virtio-blk + block；ext4/vfs 移进 U-mode `fs` | 内核不应解释路径与 inode |
| 10. 延迟终止兑现 | `resident_trap` 进入时先检查 `sched_thread_kill_pending` | RUNNING 线程无法当场摘除，需要一个确定的退出点 |

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

`BootInfo` 校验通过后，`bootstrap_run` 先校验清单中每个描述符（`name` / `run` 非空），再逐条执行。内核 demo 在 S-mode 运行，用户态 demo 进入 U-mode，完成后回到启动执行流。每项输出 `[模块名] start` 和 `[模块名] PASS`。全清单完成后输出 `[bootstrap] PASS`；默认 `capability` 镜像随后进入 `kernel_enter_resident()` 并常驻，其他 `BOOT_MODULES` 清单保留显式关机以便自动验收。模块断言失败会打印失败项并以非零状态退出。

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
[bootstrap] preempt: 1 ms SBI timer armed on unified trap path
[bootstrap] preempt: worker-a starts without yielding
[bootstrap] preempt: worker-b starts before worker-a done
[bootstrap] preempt: worker-a resumed after timer switch
[bootstrap] preempt: worker-b resumed after timer switch
[bootstrap] preempt: ticks=61 switches=7 quantum_expired=5 workers=2
[bootstrap] PASS
[kmain] pool: base=0x000000008026e000 bytes=131670016 frames=32146 reserved=2 free=32144
[kmain] reserve: firmware 0x0000000080000000 +2097152
[kmain] reserve: kernel 0x0000000080200000 +450560
[kmain] reserve: dtb 0x0000000087e00000 +5398
[kmain] idle: pool=0x000000008026e000+32146 frames free=32074
[kmain] idle: heartbeat free=32074 ext_irqs=0 timer_ticks=2001
```

具体数值会随内核大小和 QEMU 版本略变；关键是四段：`entry` 断言、`reserved`
列表、`arch` 证据，以及默认镜像的 timer 抢占计数。能力自检的完整输出见
[`kernel/capability/README.md`](../capability/README.md)。默认抢占的完整串口见
[`qemu-default-preempt.log`](qemu-default-preempt.log)。

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
| `worker-b starts before worker-a done` | 两条不同内核线程交替运行：worker-a 尚未完成时被 timer 抢占，worker-b 获得 CPU |
| `worker-a/b resumed after timer switch` | 两条线程都被时间片切走，并在 timer 抢占后恢复（判别性证据） |
| `ticks=60 switches=7 quantum_expired=4` | 辅助计数来自本次 QEMU 运行；单独不构成抢占证明，需与 overlap/resumed 联合看 |
| `[bootstrap] PASS` | 清单各项和默认抢占探针均已按顺序结束 |

### QEMU 实测证据（启动闭环）

实测命令、退出码与串口记录如下（均在仓库根目录执行）：

| 场景 | 命令 | 退出码 | 完整输出 |
|---|---|---|---|
| M1 无 initrd 常驻复核 | `timeout 8s qemu-system-riscv64 ... -kernel micro/build/riscv/kernel.elf` | 124（外部 timeout） | [`../../docs/evidence/M1-2026-11-24.log`](../../docs/evidence/M1-2026-11-24.log) |
| U-mode ecall 单模块 | `make -C micro run BOOT_MODULES=libs` | 0 | [`qemu-boot-umode.log`](qemu-boot-umode.log) |
| 默认抢占历史基线 | `make -C micro run`（M1 前） | 0 | [`qemu-default-preempt.log`](qemu-default-preempt.log) |

早期 log 开头的 `# command:` / `# exit code:` 和最新 Goal log 的 `$ ...` / `EXIT=...` 均来自实际执行。OpenSBI 横幅带 NUL 字节，历史日志保存前仅用 `tr -d '\000'` 做纯文本化。默认镜像自 M1 起不再主动 `sbi_shutdown()`；非 resident 单模块/all 清单仍保留该可控关机路径。

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

### 默认镜像的 timer 抢占接线（本次新增）

架构核实结论：

- 默认 `make -C micro run` 的 `BOOT_MODULES` 是 `capability`。正式镜像链接
  `shared/demo.c` 提供的 `trap_handler`，**不链接** `kernel/arch/riscv/trap.c`；
  后者只属于 `ARCH_SELFTEST` 自检镜像。
- `clock` 的 one-shot 队列、`sched_start_tick()`、`sched_tick()/sched_check()`
  和 `shared/demo.c` 的 timer glue 此前已经存在；但 `sched_start_tick()` 只在
  `kernel/syscall/closure.c` 与 `kernel/sched/demo.c` 调用，所以默认 capability
  镜像没有周期 tick，也没有抢占证据。

本次把最后一段启动接线补在正式路径上，而不是复制一个模块 demo：

| 改动 | 位置 | 作用 |
| --- | --- | --- |
| 仅默认 `capability` 配置链接抢占探针 | `micro/Makefile` 的 `ifeq ($(BOOT_SELECTED),capability)` | 不改其它模块清单的既有行为 |
| 模块清单之后调用探针 | `kernel/boot/bootstrap.c` 的弱符号 `boot_preempt_probe` | 探针属于 bootstrap，不属于 capability demo |
| 创建两条内核 worker、`clock_init()`、`sched_start_tick(1 ms)`、开 `SIE` | `kernel/boot/preempt_probe.c` | 真实 timer 中断进入 `shared/demo.c` 的统一 trap |
| timer IRQ 路径 | `shared/demo.c` → `clock_handle_interrupt()` → `sched_tick()` → `sched_check()` → `sched()` → `cpu_switch()` | 产生真实 tick、时间片耗尽和上下文切换 |
| 收尾核验 | `preempt_probe.c` | 检查两条 worker 都启动/完成、出现 overlap、都被切走再恢复，并打印 tick/switch/quantum |

边界：

- 这个探针只证明“默认镜像的 S 态 timer 中断能在统一 trap 路径上触发抢占”，
  不是完整的 idle/服务调度策略，也不创建首个用户态服务。它是 boot 在默认
  启动上的**验证专用例外**；boot 自身仍不创建首个线程/服务。
- 只在 `BOOT_SELECTED` 恰好等于 `capability` 时链接。`BOOT_MODULES=capability`
  可独立通过；`syscall`、`sched`、`clock` 等配置保持原有验收输出。
- 有判别力的是 `worker-b starts before worker-a done` 和两条
  `resumed after timer switch`；`ticks/switches/quantum_expired` 是辅助计数，
  单独不能证明抢占。worker 的启动/恢复日志在抢占点前后可能交换顺序，验收
  不依赖固定行序。

完整证据见 [`qemu-default-preempt.log`](qemu-default-preempt.log)，实测退出码为 0，
关键行为：

```text
[capability] PASS
[bootstrap] preempt: worker-a starts without yielding
[bootstrap] preempt: worker-b starts before worker-a done
[bootstrap] preempt: worker-a resumed after timer switch
[bootstrap] preempt: worker-b resumed after timer switch
[bootstrap] preempt: ticks=60 switches=7 quantum_expired=4 workers=2
[bootstrap] PASS
[kmain] shutting down
```

---

## 八、显式不做与已知限制

| 限制 | 现状 | 归属 |
|---|---|---|
| 单 hart | 只用 hart 0；其他 hart 在 `_start` 直接 `wfi` 停车 | 后续 SMP 切面 |
| DTB 节点解析 | 只验头，用 `platform.h` 常量 | `kernel/platform/fdt.c` |
| 自举模块来源 | 单模块 demo 仍构建进内核 ELF；resident 初始用户程序由独立 ELF + initrd 加载 | `bootstrap_plan.c` / `kernel/loader` |
| 物理页分配器 | resident 从 BootInfo RAM 建真实帧池；单模块 demo 保留 static 池 | `kernel/memory` / `kernel_init.c` |
| 首个线程/服务 | idle 与 resident 用户镜像经真实线程/AS 路径创建；服务化验收见 M3 | `kernel/boot` / `userland` |
| 默认抢占探针 | 验证 timer IRQ 抢占两条内核 worker；resident idle 另有周期 tick | `kernel/boot/preempt_probe.c` |
| `kmain` | 默认镜像进入常驻；其他清单完成 demo 后显式关机 | `kernel/boot/boot.c` / `kernel_init.c` |
| context/trap | 每次自举使用共享切换和陷阱入口，架构自检有独立镜像 | `arch/riscv/README.md` §八 |
| MBR/PBR | RISC-V virt 不存在这条链 | `ex/x86-bios` |

`BootInfo` 分配在启动栈上。各 demo 运行期间，启动栈由 `context_switch` 保存，清单结束后恢复。

---

## 九、下一步

1. **`kernel/platform/fdt.c`**：解析 `/memory`、`timebase-frequency`、`stdout-path`、`initrd`，让 `BootInfo` 不再依赖 `platform.h`。
2. **初始用户态 ELF**。在启动契约中记录独立用户镜像的物理区间，由内核据此建立用户地址空间。
3. **SMP**：hart lottery、per-hart `hart_local` 数组、非 boot hart 的 `wfi` 停车场。
4. **真正的 `kmain`**：接 `kernel/objects` 创建首个地址空间/线程，或接 `userland/init` 完成第一次 IPC。
## M7 resident 启动接线

- `service_boot_prepare` 创建 `ep_sm/ep_pm/ep_fs/ep_console/ep_init_ready`；SM 持有
  PM/FS/console/VM 的 `WRITE|GRANT` 源 cap；init 拿到 SM recv/send、ready recv
  和标量 `plan=3`（镜像表 {fs,console,shell} 内置在 init 中）。
- **大块数据**：没有共享 IPC 页。FS 文件内容按 24 B/块的 IPC 消息返回，
  cat 路径按 16 B/块传入；console 输出同样经 IPC 分块。
- **设备 MMIO**：`loader_spawn_user()` 在 `as_share_kernel(...,2)` 后把 UART0、
  PLIC、virtio-mmio 恒等映射进每个用户 AS（无 U 位）。否则内核代 U-mode 执行
  FS 读取时访问 virtio 会缺页，错误打印再访问 UART 会形成静默挂死（M7 根因）。
- PM spawn FS/console 时安装 ready send（slot 1）、SM send（slot 3）、服务 recv
  （slot 2）；spawn shell 时只安装 ready send 与 SM send，FS/PM/console 句柄由
  shell 经 SM lookup 获得。
- **M7 服务链**：内核 spawn `sm/pm/vm/heaptest/heaptest2/init`；init `plan=3` 经 PM 拉 fs/console/shell（initrd 9 成员）。**勿**默认链 `kernel_init_m6.c` / `service_boot_m6.c` / `init_main_m6.o`；`make check` 运行 `tools/verify_capability_wiring.sh`。
- PM-only plan=6 实验见 `kernel_init_m6.c` 等，未作验收默认。
