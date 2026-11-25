# [M7] 综合验收：init → shell → 关机

**依赖**：M5、M6  
**建议 Agent**：A7  
**波次**：6

## 背景

前面 7 个 Goal 各自有一条验收命令。但"这是一个操作系统"的判据不是"每个部件都有 log"，而是**一条命令启动后，用户能输入命令并得到正确结果**。

现状：`kmain` 跑完 demo 就关机（M1 改掉）；没有 shell；没有输入路径（M4 补上）；没有文件读取（M5 补上）；`userland/apps/` 只有一个 echo 客户端模型。

## 教材对照

| 王道 | 汤小丹 |
| --- | --- |
| 1.2 操作系统的发展（CPU 利用率、多道对比） | 第 1 章 |
| 综合：1–5 章的实物对照 | 综合 |

## 目标

1. **`micro/tools/demo.sh`**（新）：一条命令完成全部验收
   ```sh
   ./micro/tools/demo.sh
   ```
   内容：构建 → `make user` → `make fs-image` → 启动 QEMU（带 initrd + drive）→ 通过 stdin 依次喂入 `help`、`cat /motd`、`ps`、`echo hello`、`poweroff` → 抓 stdout → 逐条断言关键行 → 打印 PASS/FAIL 与退出码。
   **必须能非交互跑**（CI 友好），但不许用"改内核让它自动打印答案"的方式伪造——喂的是真实 stdin。
2. **shell**（`userland/apps/` 或新 `userland/shell/`）：
   - 提示符 `micro> `；
   - `help` 列命令；`cat <path>` 经 VFS；`ps` 经 PM 列进程；`echo <args>`；`poweroff` 走 SBI `SRST`；
   - 未知命令打印 `unknown: <cmd>`，不 panic。
3. **`ps` 有真实数据**：从 PM 拿进程表（M3），不是硬编码列表。至少能看到 init / sm / pm / fs / shell 五个。
4. **关机路径干净**：`poweroff` → 所有 U-mode 服务被通知 → 内核 `sbi_shutdown()`。日志要有 `[kmain] shutdown requested by shell`。
5. **README 收口**：
   - `micro/README.md` 状态表全部刷新为真实状态；
   - `micro/docs/goals/README.md` 的「基线事实」表加一节「M0–M7 完成后」；
   - 根 `README.md` 的「微内核」状态从"骨架 + 最小启动模型"改为实际状态；
   - 每个模块 README 的「已知边界」逐条复核：已解决的**删掉**，未解决的**保留并写明归属 Goal**。

## 交付

- `micro/tools/demo.sh`（可执行）
- `userland/shell/`（或 `userland/apps/shell.c`）
- `micro/Makefile`（`demo` 目标）
- `micro/README.md`、`micro/docs/goals/README.md`、根 `README.md`
- 至少 10 个模块 README 的「已知边界」复核 diff
- `docs/design/M7-integration.md`、`docs/evidence/M7-<日期>.log`（demo.sh 的完整输出）

## 验收

```bash
./micro/tools/demo.sh; echo "EXIT=$?"
# 期望 EXIT=0，且输出含：
#   [demo] help       ok
#   [demo] cat motd   ok (sha256=<hex>)
#   [demo] ps         ok (5 procs)
#   [demo] echo       ok
#   [demo] poweroff   ok
#   [demo] PASS
```

回归（一条都不许退）：

```bash
make -C micro check                                            # M0
make -C micro/kernel/{objects,memory,sched,ipc} test           # host
make -C micro run BOOT_MODULES=capability                      # 默认镜像仍通过
make -C micro run BOOT_MODULES=all                             # 20 模块仍全 PASS
make -C micro/shared check                                     # 13 单模块镜像
```

## 非目标

- 不做管道 / 重定向 / 环境变量 / job control
- 不做 bash 语法兼容
- 不做网络命令（net 仍是模型，另开 Goal）
- 不做多用户、权限、登录

## 已知风险

- **`demo.sh` 的最容易退化成"假验收"**：如果 shell 读的是脚本而不是 stdin，或者断言的是"内核打印了某行"而不是"shell 真的执行了"，这个 Goal 就白做了。判据：把 `cat /motd` 改成 `cat /nonexistent`，`demo.sh` **必须失败**。
- **时序**：QEMU 启动到 shell 就绪的时间不固定。`demo.sh` 要轮询等待提示符而不是 `sleep 3`，否则在慢机器上 flaky。
- **输出缓冲**：UART 是同步的，但 `demo.sh` 抓管道时 stdio 会全缓冲。要确保内核/用户侧不依赖 `\n` flush 行为（UART 驱动直接写寄存器就没这问题）。

## 纪律

必读：王道 1.2 与 1–5 章综合；汤小丹第 1 章与综合。  
冲突时：以模块 README 不变量为准；README 与实现冲突时先改 README 并说明。  
完成后：只提交本 Goal 范围 diff，附 `docs/evidence/M7-<日期>.log`（demo.sh 完整输出 + 五个回归命令的结果）。
