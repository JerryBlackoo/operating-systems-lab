# G6 分页与 MMU（内核映射）

- 王道：3.1 内存管理概念
- 汤小丹：第 6 章 存储器管理

## 实现

- 公共 API：`vm_init/vm_map/vm_unmap/vm_translate/vm_switch`，架构实现在 `kernel/arch/{riscv,arm}/vm.c`。
- RISC-V：Sv39 三级页表；内核 1 GiB 恒等映射 `0x80000000`，UART `0x10000000` 单独 4 KiB 映射；`satp` 切页表并 `sfence.vma`。
- ARM：短描述符 L1（16 KiB，4096 项）；DDR `0..0x20000000`、UART/GIC 以及用户代码/栈用 1 MiB 段；L1 类型 `0b01` 可挂 1 KiB 粗页表（256 项），每项映射 4 KiB 小页。`TTBR0/TTBCR/DACR` + `SCTLR.M/I`。
- `vm_selftest`：两架构都映射一个 4 KiB 页、写入、读回、取消映射。
- 页表切换在 `sched()` 中完成，内核恒等映射在所有用户页表中都保留。

## 取舍与边界

- RISC-V 内核只映射 RAM 1 GiB 和 UART，不解析设备树。
- ARM 短描述符的域字段：L1 段/页表统一用 domain 0，`DACR` 置 1（client），由描述符 AP 位做权限区分。
- **AP 位位置陷阱**：段描述符的 `AP[1:0]` 在 bit[11:10]、`AP[2]` 在 bit[15]；页描述符（小页/大页）的 `AP[1:0]` 在 bit[5:4]、`AP[2]` 在 bit[9]。最初照段描述符把 AP 写到 bit[11:10]，导致小页的 AP 恒为 `0b00`（无访问权限），`vm_selftest` 报 DFSR=`0x80f`（permission fault, page）。按 ARM ARM B3.5.3 / QEMU `get_phys_addr_v6` 修正后通过。
- ARM 当前只开 MMU + I-cache，D-cache 关闭；页表一致性留待后续。
- 不做大页策略、LPAE、五级页表。

## 验收

```bash
make && timeout 10s make run
# [vm] Sv39 enabled ...
# [vm] selftest map/unmap ok

make ARCH=arm && timeout 12s make ARCH=arm run
# [vm] selftest map/unmap ok (ARM 4KiB)
```

## 已知限制

- RISC-V 页表中间层泄漏（只回收数据页，不回收页表页）。
- 没有按需页表回收和地址空间销毁。
