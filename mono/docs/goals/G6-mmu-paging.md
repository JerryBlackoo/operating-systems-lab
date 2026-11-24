# [G6] 分页与 MMU（内核映射）

**依赖**：G1  
**建议 Agent**：A4

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 3.1 内存管理概念 | 第 6 章 存储器管理 |

## 目标

1. **RISC-V**：启用 Sv39；内核恒等映射或高半映射（与 `0x80200000` 链接一致）。
2. **ARM**：短描述符二级页表；映射 DDR 与设备 MMIO（UART、GIC）。
3. `walk_pte` / `map_page` / `unmap_page`；与现有 `pmm.c` 4KiB 页对接。
4. 启用 D/I cache 前后屏障与文档说明（Z7 实机相关见 G14）。

## 交付

- `kernel/mm/vm.c`, `pte.h`, arch 页表汇编辅助
- `docs/design/G6-mmu-paging.md`

## 验收

```bash
make && make run
make ARCH=arm && make ARCH=arm run
# 期望：MMU 开启后仍完整 boot；[vm] selftest map/unmap ok
```

`vm_selftest`：映射一页、写入、读回、取消映射。

## 非目标

- 用户进程独立页表（G7）
- 大页 / 五页表 ARM LPAE

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。