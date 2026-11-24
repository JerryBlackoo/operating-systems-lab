# [G7] 用户虚拟地址空间

**依赖**：G6、G2  
**建议 Agent**：A4

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 3.1 内存管理概念（程序装入、地址空间） | 第 7 章 虚拟存储（布局） |
| 3.2 虚拟内存管理（前半） | 程序链接装入、伙伴系统可选不提 |

## 目标

1. 每进程 `pagetable_t`：代码 / 数据 / 堆 / 栈 区域（固定上限教学布局即可）。
2. `sys_exec`：加载 G1 用户 ELF 或 flat 镜像到新地址空间。
3. `sys_fork`：**最小实现**——可「完全复制页表」；COW 标为 TODO 须在 design 写明。
4. `sys_sbrk` 或最小 `brk` 扩展堆一页。

## 交付

- `kernel/mm/uvm.c`, `kernel/proc/exec.c`, `kernel/syscall/` 扩展
- `docs/design/G7-user-vma.md`：用户区示意图对照王道/汤图

## 验收

```bash
make && make run
# 期望：启动两用户程序或 fork 演示，debug 打印两进程同虚拟不同物理页帧号
```

## 非目标

- 完整 `mmap` 文件映射（可与 G10 后接）
- 写时复制完整实现

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。