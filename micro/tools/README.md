# tools

验收与打包脚本。它们不是库，只被 `make` 目标或开发者手工调用。

## 本目录的对象

| 脚本 | 作用 | 被谁调用 |
| --- | --- | --- |
| `demo.sh` | M7 一条命令端到端验收（正向 + 负向） | `make demo`、`make check` |
| `modules_qemu.py` | **逐模块 QEMU 验证**：20 个模块各自单独启动并判定 PASS；常驻镜像（capability）看到 PASS 即提前退出 | `make check` |
| `shell_smoke.py` | **交互输出完整性**烟雾测试：逐条命令的完整输出 + help 整行未被交错 | `make check` |
| `shell_stress.py` | 长会话稳定性：N 条命令后 IPC 资源不泄漏 | `make check`（20 条）、手工（60 条） |
| `m5_discriminate.sh` | M5 判别：改镜像后 motd 哈希必变；200 KiB largefile 全量哈希 | 手工 |
| `m6_discriminate.sh` | M6 判别：两个进程同 VA 落到不同物理帧；越权 store 被拒 | 手工 |
| `console_irq_test.py` | M4 UART IRQ 行缓冲判别 | 手工 |
| `mkext4image.py` | 生成只读 ext4 镜像（`/motd` + 200 KiB `/largefile`） | `make fs-image` |
| `mkinitrd.py` | MIRD v1 initrd 打包（9 个 ELF 成员） | `make initrd` |
| `verify_capability_wiring.sh` | 防误链：检查 capability 镜像必含 `kernel_init.c`/`service_boot.c`、不含 `*_m6.c` | `make check` 第一步 |

## 与其他模块的关联

| 对象 | 关系 |
| --- | --- |
| `Makefile` | `check` 目标串联本目录的脚本；`demo`/`fs-image`/`initrd` 依赖打包脚本 |
| `kernel/boot` | `mkext4image.py` 产出的镜像由内核 `virtio_blk` 读取；`mkinitrd.py` 的产物由 `loader_find_initrd` 解析 |
| U-mode `fs` | `[fs] motd sha256=` / `[fs] largefile sha256=` 这些行由 **U-mode fs 的自检**打印（A1 后不再由内核打印），`demo.sh` 与 `m5_discriminate.sh` 都依赖它们 |
| `userland/shell` | `demo.sh` / `shell_smoke.py` / `shell_stress.py` 都通过真实的 QEMU stdin 驱动它 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 只有 `mkext4image.py` | 造一个带 `/motd` 的 ext4 镜像 | M5 需要一个真实文件系统 |
| 2. 加 200 KiB `/largefile` | 大文件跨多个 extent 叶块 | 小块能过不代表大文件能过；需要全量哈希 |
| 3. `mkinitrd.py` | MIRD v1 打包 9 个 ELF | loader 需要一个容器格式 |
| 4. `demo.sh` 固定 `sleep` | 早期用固定等待 | **不可靠**：QEMU 启动时间随负载波动，固定 sleep 会假失败 |
| 5. 改为 stdout 轮询 | 所有等待都是「轮询 + 超时」，不 sleep | 让验收快且稳定 |
| 6. `--expect-nonzero` | 负向模式：`cat /nonexistent` 必须返回 not found 且脚本 exit 1 | 证明正向断言不是「无条件 PASS」 |
| 7. `m5`/`m6` 判别脚本 | 不是「跑通」而是「能区分」：改镜像哈希必变；同 VA 必须不同帧 | PASS 太弱，需要能证伪的断言 |
| 8. `shell_stress.py`（A5） | 同一次启动连发 N 条 `help` | 异步 IPC 的 reply 槽位泄漏表现为「第一条命令就静默」，单条命令的 demo 永远发现不了 |
| 9. `shell_smoke.py`（A31） | 逐条命令断言完整输出 + help 整行未被其它服务日志交错 | `make check` 曾约 1/8 概率失败，因为 fs 自检输出插进了 help 中间 |
| 10. `verify_capability_wiring.sh` | 检查 Makefile 的 `C_SRCS` 是否误链 `*_m6.c` | 曾经默认镜像误链 M6 实验路径，导致启动行为与文档不符 |

## demo.sh（M7 一条命令验收）

```sh
./micro/tools/demo.sh                  # 正向验收，成功 exit 0
./micro/tools/demo.sh --expect-nonzero # 判别性负向，exit 1
```

脚本会真实执行：

```text
make -C micro user initrd fs-image
make -C micro BOOT_MODULES=capability
qemu-system-riscv64 ... -initrd ... -kernel ...
```

然后通过 QEMU stdin 轮询 `micro> `，依次发送：

```text
help
cat /motd
ps
echo hello
poweroff
```

断言：

- **U-mode `fs` 自检**打印的 `[fs] motd sha256` 与镜像内 `/motd` 的 sha256 一致
  （A1 之后这一行由 U-mode 产生，不再由内核打印）；
- shell `cat /motd` 输出被 demo 按标记截取后再次做 sha256，必须与镜像一致；
- `ps` 至少 5 条记录且包含 init/sm/pm/fs/console/shell；
- `echo hello` 输出 hello；
- 只在看到 `[kmain] shutdown requested by shell` 后接受关机并等待 QEMU 退出。

负向模式把 `cat /motd` 换成 `cat /nonexistent`（`--expect-nonzero` 是
`--cat-path /nonexistent` 的别名），要求 FS 返回明确 not found，且脚本自身
`exit 1`，证明正向断言不是无条件 PASS。串口侧应含
`cat: /nonexistent: not found`。

不使用固定 `sleep` 等待；所有等待均为对 stdout 的轮询/超时。

## modules_qemu.py

```sh
python3 tools/modules_qemu.py          # 逐个跑 20 个模块
python3 tools/modules_qemu.py --list   # 只列出模块名
```

对每个 `BOOT_MODULES=<m>` 单独启动 QEMU，要求出现该模块自己的 PASS 行
（`[<m>] PASS` 或 `<m>: all tests passed`），且没有 `demo_fail` / `Assertion`
/ `panic`。

**常驻镜像的特殊判定**：`capability` 跑完 demo 后进入 idle 心跳、不会自行关机，
靠外层 timeout 收尾是预期行为。脚本对它在看到 PASS 后立即 kill QEMU，
所以从约 45 秒降到 2.3 秒。

这个脚本存在的理由：`BOOT_MODULES=all` 能证明「20 个模块链接在一起能跑」，
但**不能**证明「每个模块单独启动也能跑」——后者要求该模块的 demo 不依赖
其它模块的初始化副作用。

## shell_smoke.py
```sh
python3 tools/shell_smoke.py
```

逐条执行 `help` / `cat /motd` / `cat /nonexistent` / `echo hello-world-42` / `ps`，
每项断言各自的完整输出，最后再断言 **help 的整行没有被其它服务的日志插入**。
这条断言守护 A31（分阶段 spawn），因为它是最难通过单次运行发现的一类缺陷。

## shell_stress.py

```sh
python3 tools/shell_stress.py 20   # make check 用 20
python3 tools/shell_stress.py 60   # 手工长跑
```

同一次启动里连发 N 条 `help`，每条都要看到完整的 `commands:` 行与下一个
`micro> `。它守护 A5（reply 槽位泄漏）与 A31（输出交错）。

## 判别脚本

```sh
bash tools/m5_discriminate.sh   # 改镜像后 motd 哈希必须变；largefile 全量哈希
bash tools/m6_discriminate.sh   # 同 VA 不同物理帧；越权 store 被拒 status=-4
```

它们的共同点：**不只验证「能跑」，而是验证「能区分」**。`m5` 会重写镜像后
重跑，哈希必须随之变化；`m6` 要求两个进程访问同一 VA 得到不同物理帧。
