# M1 内核常驻

- **日期**：2026-11-24
- **教材**：王道 1.4–1.5、2.1；汤小丹 1.4、2.2

## 实现

| 项 | 位置 |
| --- | --- |
| demo 后常驻 | `kernel/boot/boot.c`：`MICRO_POST_BOOT_RESIDENT` 时调用 `kernel_enter_resident` |
| 真帧池 | `kernel/boot/kernel_init.c`：`[align_up(kernel_end), ram_end)` + 各 `bootinfo` 保留区 `frame_reserve` |
| idle | `thread_create` + `sched_set_idle` + `cpu_switch_to`；`wfi` + 2s 心跳（`clock`） |
| 编译开关 | `micro/Makefile`：仅 `BOOT_SELECTED=capability` 链入 `kernel_init.c` 并 `-DMICRO_POST_BOOT_RESIDENT=1` |
| 帧查询 API | `frame_total_count()` / 既有 `frame_free_count()` |

## 帧池与直接映射

`frame_init(pool_base, pool_size, (void *)pool_base)`：当前 satp=0，物理地址即内核可访问 VA。

## initrd 预留与 M2/M3 接线

`fdt_initrd.c` 解析 `/chosen` 的 `linux,initrd-start/end`；若存在则记入 `BOOT_REGION_INITRD` 保留区。当前默认镜像已由后续 Goal 消费 initrd；独立模块 demo 不使用该路径。

## 验收

```bash
timeout 12s make -C micro run; echo EXIT=$?   # 124，无 shutting down
make -C micro check                            # 仍 EXIT=0（all 清单关机）
```

证据：`docs/evidence/M1-2026-11-24.log`。独立复跑不携带 initrd/块设备，排除后续 Goal 干扰；实测 `EXIT=124`，`reserved=2`，3 次心跳的 `timer_ticks` 分别为 2001、4002、6003。

集成复核补充见 `docs/evidence/M1-2026-11-24-recheck.log`：当前默认系统镜像在挂载 initrd/只读块设备后仍进入 idle，3 次心跳为 2000、4001、6002，`EXIT=124`；强制重建 memory host 自测、完整 `make check` 和 `__kernel_end=0x8026e000` 页对齐均通过。