# [G8] 缺页中断与页面置换

**依赖**：G7  
**建议 Agent**：A5

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 3.2 虚拟内存管理 | 第 7 章 虚拟存储（请求分页） |

## 目标

1. 注册页故障处理：读/写/执行非法 → 杀进程或扩展堆；**按需分配**零页。
2. 物理页不足时：**Clock** 或 **FIFO** 置换一种；可只驱逐「可回收用户页」。
3. 统计：`page_fault_count`、`evict_count`；串口 `kprintf` 或 debug syscall 读出。
4. 可选：交换区存于 RAM 盘固定槽位（非必须写 SD）。

## 交付

- `kernel/mm/trap_vm.c`, `kernel/mm/swap.c`（可选）
- `docs/design/G8-page-fault.md`：选用算法与王道例题对照

## 验收

```bash
make && make run
# userland 测试：分配大于可用物理页的数组，触发 fault
# 期望：[vm] faults=... evictions=... 且不死机
```

缩小 `PHYS` 池做自测时在 design 文档记录临时改动的宏。

## 非目标

- 工作集、Thrashing 仿真整套
- 内核页换出

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。