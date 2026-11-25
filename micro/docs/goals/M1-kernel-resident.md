# [M1] 内核常驻：真实帧池与 idle

**依赖**：M0  
**建议 Agent**：A1  
**波次**：2

## 背景

现在 `micro` 的默认镜像跑完 demo 就 `sbi_shutdown()`（`kernel/boot/boot.c:208` 的 `kmain`）。而且**没有任何一处用真实 RAM 建立帧池**：`frame_init()` 的调用者全是 demo 里对 `static _Alignas(4096) uint8_t kernel_frames[...]` 数组调用（`kernel/memory/demo.c:15`、`kernel/syscall/demo.c:636`、`userland/servers/{pm,vm}/demo.c`）。也就是说"物理内存管理"目前只管一块编译期数组。

这导致后面所有 Goal 都无法成立：没有真帧池就没有真 AS，没有真 AS 就没有 initrd、没有服务、没有文件系统。

## 教材对照

| 王道 | 汤小丹 |
| --- | --- |
| 1.4 操作系统引导 | 1.4 操作系统引导与运行 |
| 1.5 中断、异常和系统调用（trap 常驻） | 1.5 系统调用 |
| 2.1 进程的概念（内核 idle 线程） | 2.2 进程控制 |

## 目标

1. **从 BootInfo 建真帧池**：用 `bootinfo.regions` 里的 `BOOT_REGION_RAM` 减去 `KERNEL` / `DTB` / `FIRMWARE` 保留区，`frame_init()` 到 `[__kernel_end 向上对齐, RAM 顶)`。启动日志必须打印：帧池物理区间、总帧数、保留帧数。
2. **保留区正确**：`frame_reserve()` 覆盖 OpenSBI 固件区、内核镜像 `[__kernel_start, __kernel_end)`、DTB（若 BootInfo 记了 size）。日志逐条打印保留区，便于人工核对。
3. **内核常驻**：`kmain` 不再无条件 `sbi_shutdown()`。改为：
   ```text
   boot → bootstrap_run()（现有模块清单仍照跑）
        → kernel_init()：object_system_init → frame_init → trap_install 复核
        → 创建 idle 线程 → sched_bootstrap()
        → idle 循环（wfi），永不返回
   ```
   `idle` 必须打印一次 `[kmain] idle: pool=<base>+<n> frames free=<n>`，然后持续 `wfi`。
4. **可控关机**：新增一个显式的关机路径（例如 SBI `SRST` 由 idle 在收到某种退出条件时调用，或保留 `BOOT_MODULES` 全 demo 模式下仍可在末尾关机）。**默认镜像不许关机**，这样 `timeout` 才是真正的"仍然活着"证据。
5. **`__attribute__((noreturn))` 语义正确**：`kmain` 之后的代码路径不得假设会返回；`bootstrap_run()` 的调用点要注释清楚"demo 阶段已结束"。

## 交付

- `kernel/boot/boot.c`（`kmain` 重写为不返回）
- 新增 `kernel/boot/kernel_init.c` / `.h`（帧池构建 + 保留区 + idle 线程创建）
- `frame.c` 如需新 API（例如 `frame_total/frame_free` 查询），加在 `memory.h` 并更新 `kernel/memory/README.md` 的 API 表
- `micro/README.md` 状态表、`docs/design/M1-kernel-resident.md`
- `docs/evidence/M1-<日期>.log`

## 验收

```bash
make -C micro && timeout 10s make -C micro run; echo "EXIT=$?"
# 期望：EXIT=124（timeout 杀掉，说明内核仍在跑），且日志含
#   [kmain] pool: base=0x... frames=NNNN
#   [kmain] reserve: firmware/kernel/dtb ...
#   [kmain] idle: pool=... free=NNNN
# 不得出现 [kmain] shutting down
```

补充验收（防止"假常驻"）：

```bash
# idle 必须真的在跑：连续两次采样 tick 计数应递增
make -C micro run BOOT_MODULES=capability,clock  # 若该组合可行
# 或在 idle 里每 N 次 wfi 打印一次心跳，日志中出现两行以上心跳
```

帧池正确性另加一条 host 断言路径（可选但推荐）：在 `memory_test.c` 里用与 `kernel_init` 相同的"RAM 减保留区"算法喂一个合成 region 表，断言算出的区间与手算一致。这样"算法错"和"硬件内存映射不同"能分开定位。

## 非目标

- 不加载用户镜像（M2）
- 不做内核堆 / `kmalloc`（当前无人需要；等 M2 的 ELF 加载器真的需要时再评估，见 M2 非目标）
- 不做 SMP、不做 per-hart 帧池
- 不改各模块 demo 里现有的 `static` 帧数组用法（它们证明模型本身，保留）

## 已知风险

- **保留区算错会静默破坏 demo**：如果把 demo 用的 `static` 数组所在 `.bss` 也当可用 RAM 分配出去，后续模块会踩到自己的数据。因此帧池必须从 `__kernel_end` 之后开始，且 `__kernel_end` 必须页对齐（`link.ld` 已有 `ALIGN(4096)`，验收时 `readelf` 复核）。
- **`timeout` 的退出码是证据的一部分**：`124` 表示"还活着"，`0` 表示"自己退了"。不许用 `|| true` 掩盖。

## 纪律

必读：王道 1.4、1.5、2.1；汤小丹 1.4、2.2。  
冲突时：`kernel/memory/README.md` 的帧所有权不变量（`as_map` 加引用、映射不等于所有权转移）为准。  
完成后：只提交本 Goal 范围 diff，附 `docs/evidence/M1-<日期>.log`（必须含 `EXIT=124`）。
