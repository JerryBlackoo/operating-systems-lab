# [G13] init、shell 与教材综合演示

**依赖**：G3、G8、G10（G7/G1 syscall 已具备）  
**建议 Agent**：A7

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 1.2 发展历程（CPU 利用率、甘特图） | 综合 |
| 2.x 进程并发观感 | 综合 |

## 目标

1. 内核 `init` 线程：挂载 G10 FS，`exec` 用户 shell。
2. 最小 shell：`help` `ps` `cat` `echo` `fork_test`（子进程打印后 exit）。
3. **王道 1.2 风格演示**：用户程序或脚本模拟两程序时间片，打印 CPU 利用率估算（单道 vs 多道对比说明即可，不追求工业精度）。
4. 更新根 `README.md`「当前状态」已实现列表。

## 交付

- `userland/init.c`, `userland/sh.c`
- `docs/design/G13-init-shell.md`

## 验收

```bash
make && make run
# 交互：输入 help 列出命令；cat /motd 成功
# 非交互：timeout 20s 内自动跑 demo 并打印 utilization≈...
```

`make ARCH=arm` 编译通过；ARM 交互可选。

## 非目标

- bash 全语法、管道重定向全套
- 网络下载

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。