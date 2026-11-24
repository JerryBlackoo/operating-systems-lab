# G7 用户虚拟地址空间

- 王道：3.1 程序装入、地址空间；3.2 虚拟内存前半
- 汤小丹：第 7 章 虚拟存储（布局）

## 用户布局（两架构共用）

```text
0x40000000  代码 / 只读数据 / 数据（flat 镜像）
0x40080000  堆起点（sbrk）
0x400ff000  用户栈页
0x40100000  栈顶
```

- 每个用户进程持有独立 Sv39 页表；用户页 U=1，内核 RAM/UART 映射同时存在。
- `sys_fork`：逐页复制父进程的代码、堆、栈，子进程与父进程在同一 VA 上拥有不同物理页；子进程 `a0=0`。
- `sys_exec`：按内嵌镜像名新建页表，映射代码和栈，修改当前 trap frame 的 `sepc/sp`，切换 `satp`。
- `sys_sbrk`：只移动 `heap_top`，物理页由 G8 的缺页处理按需分配。

## 取舍

- 不做 COW；fork 完全复制页（`docs` 明确标 TODO）。
- 不做 `mmap`、ELF 重定位、argv/envp、ASLR。
- ARM 已支持 `hello/fork/exec/shell`（1 MiB 段复制，`proc_fork` 复制代码/栈段）；`sbrk` 与按需分页仍只在 RISC-V 验收。

## 验收

```bash
make && timeout 15s make run
# [vm] fork parent pid=3 child pid=10 va=0x40080000 pa=0x.../0x...
# parent: fork returned child pid
# child: fork returned 0
```

## 已知限制

- 地址空间销毁不回收页表页和用户页（教学版泄漏）。
- `exec` 不支持路径查找，只认内嵌镜像名（`hello/forktest/faulttest/init/sh`）。
