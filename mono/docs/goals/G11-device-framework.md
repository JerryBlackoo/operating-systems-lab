# [G11] 设备独立层与驱动框架

**依赖**：G1  
**建议 Agent**：A7

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 5.1 I/O 管理概述 | 第 9 章 I/O 子系统 |
| 5.2 设备独立性软件 | 设备分配、假脱机略讲即可 |

## 目标

1. `devsw[]` 或等价：`major/minor`、`read/write/ioctl`、`init`。
2. 将现有 `uart.c` 迁入 `drivers/uart.c` 并注册为 console。
3. 伪设备：`/dev/null`（写丢弃、读 EOF）、`/dev/zero`（读零）。
4. 统一错误码与忙等待 / 睡眠的边界（console 可仍轮询）。

## 交付

- `kernel/dev/device.c`, `kernel/drivers/uart.c`
- `docs/design/G11-device-framework.md`

## 验收

```bash
make && make run
# 控制台输出正常；可选 selftest 对 null/zero 读写
```

## 非目标

- USB、DMA 引擎通用框架
- 可中断键盘

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。