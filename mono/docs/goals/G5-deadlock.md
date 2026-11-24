# [G5] 死锁教学模块（银行家算法）

**依赖**：G4  
**建议 Agent**：A3

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 2.4 死锁 | 第 5 章 死锁 |

## 目标

1. `kernel/sync/banker.c`：王道教材式 **Available / Max / Allocation / Need** 矩阵 API。
2. `banker_safety_check()` 返回安全序列或失败；不挂钩真实锁（仅 debug syscall 或启动自检）。
3. design 文档给 **一道王道 2.4 原题矩阵** 的输入与输出对照。

## 交付

- `kernel/sync/banker.c`, `banker.h`
- `docs/design/G5-deadlock.md`

## 验收

```bash
make && make run
# 期望：[banker] safe seq: P1 P3 P2 ... 或明确 unsafe
```

单元数据写在 `banker_selftest()`，与题库例题一致。

## 非目标

- 内核运行时自动死锁检测并恢复
- 资源分配图动态可视化

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。