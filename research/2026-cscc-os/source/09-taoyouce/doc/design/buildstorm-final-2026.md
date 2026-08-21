# RustMicroOS FINAL-2026 BuildStorm 内核设计与优化

> 状态快照：2026-08-18
> 正式范围：glibc，RISC-V64 + LoongArch64，`SMP=8 MEM=8G`
> 当前结论：CAgent 双架构已收口（评测机 RV/LA 10/10 全 success）；
> BuildStorm 主编译在修复 pipe 部分写重放语义后，RV/LA 本地原版官方脚本
> 全量均 `BUILDSTORM_COMPILE mode=multi ok=true`，等待评测机 8 核正式复核

本文对应官方 BuildStorm 人工评审项：

1. 问题与根因分析；
2. 修复或优化的设计实现；
3. 修改前后的实验对比；
4. AI 使用说明与可复现步骤。

不满足正式完成条件的单阶段、debug、缩内存或单核结果均明确标注为定位证据，
不作为 FINAL-2026 得分。

## 1. 官方工作负载与系统压力

BuildStorm 在 Debian glibc 用户态中运行 Rust 工具链，从源码全量编译
`arceos-helloworld`。完整路径包含：

```text
动态链接器
  → rustup / cargo / rustc
  → 多进程与多线程运行时
  → 大量 mmap、页错误、文件读写和目录操作
  → 临时产物原子 rename、链接与最终 ELF 运行
```

正式配置为 `-smp 8 -m 8G`。编译成功是时间分的门槛，计时来自 guest 内
`/proc/uptime`；项目不修改该接口或系统时钟来伪造结果。

## 2. 总体设计原则

### 2.1 通用 Linux 语义

- 不按测例名、命令行或输出内容特判；
- 不返回假成功，不吞错，不用影子状态表掩盖子系统缺陷；
- syscall 层只处理 ABI、用户拷贝和 errno，状态回归所属子系统；
- 改动进程/文件状态时同时闭合 fork 继承、exec 重置和 exit 清理。

### 2.2 双架构同源

上层进程、VFS、网络和 syscall 实现由 RV/LA 共用。MMU、陷入、平台 RTC、
VirtIO transport 等差异留在 `kernel/src/arch/` 与平台驱动中。

### 2.3 分阶段晋级

```text
toolchain → minibuild → prebuild → compile → 原版官方全量 → judge
```

阶段脚本只用于缩小故障面；最终结论必须来自原版官方脚本与 judge。

## 3. 问题、根因与修复

### 3.1 ELF 解释器与 glibc 混用

**现象**

CAgent 启动 Debian 程序时出现 `GLIBC_2.36` / `GLIBC_2.38 not found`。

**根因**

旧实现重写 ELF 的 `PT_INTERP`，同时由 init 注入旧 `/glibc/lib` 路径，导致
Debian 新动态链接器与旧 libc 混用。

**设计**

- ELF 加载优先使用文件声明的 exact `PT_INTERP`；
- 完整发行版 rootfs 中保持 `LD_LIBRARY_PATH` 为空；
- 旧比赛镜像兼容逻辑只在明确判据成立时启用。

**结果**

RV/LA CAgent 均恢复为 10/10、199.1/200。

证据：

- `local-judge/reports/sem160/rv-cagent-fix2.serial`
- `local-judge/reports/sem160/la-cagent-fix2.serial`
- `local-judge/reports/sem161-fix1/rv-cagent.serial`
- `local-judge/reports/sem161-fix1/la-cagent.serial`

### 3.2 ext4 只读基层删除与 whiteout

**现象**

BuildStorm 清理预构建目录时，`rm -rf` 无法删除镜像中的既有条目。

**根因**

正式镜像以 snapshot 模式运行，盘上 ext4 条目不能直接持久化删除；旧覆盖层没有
完整的 whiteout 语义。

**设计**

- 对盘上条目删除时在覆盖层记录 whiteout；
- `lookup`、`read_dir` 和 `create` 统一查询 whiteout；
- 删除后允许同名条目在可写覆盖层重建。

**结果**

白盒探针完成删除并清理模拟的 `libc.a`。

证据：`local-judge/reports/sem160/rv-whiteout-probe.serial`。

### 3.3 pipe 元数据与 cargo jobserver

**现象**

cargo 报告 `fd 3 is not a pipe`，并放弃继承 jobserver。

**根因**

匿名管道 `stat.st_mode` 只有权限位，没有 `S_IFIFO`；所有管道还曾共享固定 inode。

**设计**

- PipeReadEnd/PipeWriteEnd 返回 `S_IFIFO | 0o600`；
- 为每个 pipe 分配全局唯一 inode；
- 同一管道的读写端共享 inode，符合 Linux pipefs 可观察语义。

**结果**

jobserver 类型告警消失，工具链和最小构建继续向后执行。

证据：`docs/BATCH-HISTORY.md` sem160、sem161。

### 3.4 平台 RTC 与 wallclock

**现象**

cargo/rustup 报 `out of range integral type conversion attempted`；镜像中的 2026
年文件时间对内核而言来自未来。

**根因**

`CLOCK_REALTIME` 从 Unix epoch 起算，启动时没有读取平台 RTC。

**设计**

- RV 读取 Goldfish RTC 纳秒计数；
- LA 读取 LS7A RTC TOY 日历字段并执行 civil date 转换；
- 仅在 wallclock offset 尚未设置时初始化；
- 保留用户 `settimeofday` 的覆盖语义；
- 有效时间下限设为 2000-01-01，拒绝明显无效硬件值。

**结果**

双架构 guest 的 `date` 与镜像 mtime 回到真实 2026 时间域，年龄转换告警不再作为
主故障阻断流程。

实现：`kernel/src/driver/rtc.rs`。
证据：`local-judge/reports/sem160/{rv,la}-rtc-probe.serial`。

### 3.5 execve 的 de-thread 语义

**现象**

rustup 代理执行真实工具链后，Tokio worker 在 `epoll_pwait` 得到 `EBADF` 并 panic。

**根因**

线程组共享 fd 表。主线程 exec 时先执行 CLOEXEC sweep，关闭了 worker 仍在使用的
epoll fd；旧实现缺少 Linux `de_thread()`。

**设计**

`de_thread_for_exec()` 在 CLOEXEC 之前：

1. 找出同 `tgid` 的兄弟线程；
2. 按线程组退出协议解除 futex、转移 fd owner、释放资源；
3. 将兄弟线程置为终止状态；
4. 再执行 CLOEXEC、地址空间替换和新 ELF 装载。

**结果**

LA `cargo build -p tg-xtask` 从 Tokio `EBADF` panic 转为完成。

实现：

- `kernel/src/sched/process_exit.rs`
- `kernel/src/exec/process.rs`

证据：`local-judge/reports/sem160/la-ebadf-probe*.serial` 与
`la-ebadf-fix2.serial`。

### 3.6 TTY 类型与 inode 唯一性

**现象**

LA `grep -c processor /proc/cpuinfo` 无输出但退出 0。

**根因**

GNU grep 会比较 stdout 与 `/dev/null` 的 `(st_dev, st_ino)`。TTY 与 devfs 首个设备
都使用 inode 1，且旧 `st_mode` 缺少字符设备类型位，最终触发
`dev_null_output` 静音路径。

**设计**

- stdin/stdout 使用独立保留 inode；
- 补齐 `S_IFCHR` 与权限位；
- `ioctl` 的 TTY 判定与文件类型同步。

**结果**

LA 8 核 `grep -c` 输出 8，RV 无回归，CAgent 双架构仍为 10/10。

证据：`local-judge/reports/sem161-fix1/`。

### 3.7 同挂载点跨目录 rename

**现象**

rustc 原子发布 `.rmeta` 时返回 `Invalid cross-device link (EXDEV)`。

**根因**

临时文件与目标文件位于同一 ext4 挂载点的不同目录。旧
`Ext4DirInode::rename_to` 对任何跨目录操作都返回拒绝，syscall 将其映射为 EXDEV。

**设计**

- 对端目录可安全 downcast 为同一 ext4/overlay 实现；
- 在同一挂载层执行原子 `rename_to`；
- `link` 与目录类型擦除接口同步闭合；
- 仅真正跨挂载点时返回 EXDEV。

**结果**

RV/LA rename 探针均出现 `PROBE_EXDEV_RENAME ok`。

证据：`local-judge/reports/sem161/exdev-fix-evidence.md`。

### 3.8 epoll lost wakeup

**现象**

cargo/mio 在无限超时 `epoll_wait` 中永久睡眠；观察到 `wchan=ep_poll`，生产者活动
后仍无 runnable 转换。

**根因**

epoll sleeper 只进入 `EpollInner.waiters`，pipe/socket 生产者只唤醒各自底层
waitqueue，两者没有连接；同时存在“检查无事件→注册睡眠”窗口。

**设计**

1. 睡眠前向所有 armed interest fd 注册 waiter；
2. 注册后重新执行 `collect_ready`；
3. pipe 最后一个端点关闭时唤醒对端；
4. 嵌套 epoll/poll 通过 `File::register_waiter` 统一递归。

**结果**

探针得到 `EPOLL_HUP_OK events=0x11`，随后
`[SINGLE] END ... exit=0`。

证据：`local-judge/reports/sem161/epoll-hup-fix-evidence.md`。

### 3.9 pipe 部分写的 suspend-replay 重复写（主编译卡死根因）

**现象**

评测机与本地隔离复现一致：toolchain/minibuild/prebuild 全过，主编译
`cargo xtask arceos build` 打出 `Workspace root: /work/tgoskits` 后永久卡死；
更早的失败形态是 tg-xtask 读 `cargo metadata` 的 JSON 在 ~192KB 处损坏
（`expected , or } at column 196643`）。

**根因**

pipe 写端写入部分字节后遇到 `WouldBlock` 时，旧实现带着已写进度挂起并在
唤醒后**重放整个 write/writev syscall**，已写字节被重复写入下游。
`cargo metadata` 的大 JSON 经 pipe 输出被重复字节污染（探针实测 3 次输出在
3925224/4187368 字节间摆动），tg-xtask 解析失败后反复重试，表现为卡死。

**设计**

对齐 Linux pipe 语义：**有进度即短写返回**，不做 suspend + replay——

- `write` 在部分写后直接返回已写字节数，用户态 `write_all` 继续写剩余部分；
- `writev` 在部分写后立即返回累计字节数，不跳到下一个 iov；
- `O_NONBLOCK` 下无进度返回 `EAGAIN`。

实现：`kernel/src/fs/write_io.rs`。

**结果**

- 管道探针 3 次输出全部 3925224 字节且 `PIPE_JSON_VALID`（修前必损坏）；
- RV 主编译全量通过：`BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=1332.61
  cores=2 bytes=1681016 arch=riscv64`，`exit=0`；
- LA 主编译全量通过：`BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=1590.80
  cores=2 bytes=1714568 arch=loongarch64`，`exit=0`。

（本地为 2 核/6G 诊断口径，编译成功结论有效；耗时数据不作为正式耗时分，
正式口径为 8 核/8G。）

证据：
`local-judge/reports/final-compile-isolation-20260818-140242/{rv,la}-compile-fix.serial`。

## 4. 修改前后实验汇总

| 故障 | 修改前 | 修改后 | 证据级别 |
|---|---|---|---|
| CAgent 动态链接 | GLIBC version not found | RV/LA 10/10、199.1 | 正式官方脚本 + 串口 |
| ext4 删除 | `rm -rf` 失败 | whiteout 探针成功 | 最小探针 |
| RTC | 文件年龄转换溢出 | 双架构时间回到 2026 | 最小探针 |
| exec/de-thread | Tokio `EBADF` panic | LA tg-xtask 构建完成 | 阶段定位 |
| grep/TTY | LA 输出为空、exit 0 | `grep -c` 输出 8 | 8 核探针 + CAgent 回归 |
| `.rmeta` rename | EXDEV | RV/LA rename 探针成功 | 最小探针 |
| epoll HUP | 永久睡眠 | `events=0x11`、exit 0 | 最小探针 |
| pipe 部分写重放 | metadata JSON 损坏/主编译卡死 | RV/LA compile ok=true、exit 0 | 原版官方脚本全量 |

这些结果证明具体语义缺陷已被移动或闭合，但不等于 BuildStorm 正式完成。

## 5. 当前正式状态

最新证据（2026-08-18）：

- 评测机（v4 提交，官方口径）：RV/LA CAgent 10/10 全 success；
  BuildStorm toolchain OK、minibuild OK；主编译卡死点即 3.9 的 pipe 根因；
- 本地（pipe 修复后，原版官方脚本全量）：RV/LA 均
  `BUILDSTORM_COMPILE mode=multi ok=true`、产物 ≥500KB、
  `[SINGLE] END ./buildstorm_testcode.sh exit=0`；
- 本地为 2 核/6G 诊断口径，编译成功结论有效，正式耗时分以评测机 8 核为准。

## 6. AI 使用说明

AI 辅助用于：

- 搜索跨模块调用链和状态所有者；
- 从长串口中提取候选首错点；
- 设计一次性探针、crash capsule、差分实验和复验脚本；
- 检查文档中的时间线、路径和 marker 一致性；
- 生成可复现的 Marp/视频文本流水线。

AI 不作为结论来源。每项修复必须经过：

1. 源码与 Linux/POSIX 语义核对；
2. 真实 QEMU 运行；
3. 原始串口 marker 与退出码检查；
4. 受影响双架构回归；
5. 正式阶段或官方全量晋级。

所有实验保留 RunId、脚本和原始输出，审核者可以独立复核。

## 7. 可复现步骤

### 7.1 构建正式内核

```sh
bash local-judge/scripts/in-docker.sh make all
```

### 7.2 审计 pristine 镜像

```sh
bash local-judge/scripts/in-docker.sh \
  bash local-judge/scripts/final-2026/audit-pristine-image.sh
```

### 7.3 分阶段验证

```sh
bash local-judge/scripts/final-2026/bs-phase-toolchain.sh --arch both
bash local-judge/scripts/final-2026/bs-phase-minibuild.sh --arch both
bash local-judge/scripts/final-2026/bs-phase-prebuild.sh --arch both
bash local-judge/scripts/final-2026/bs-phase-compile.sh --arch both
```

### 7.4 原版官方全量

```sh
bash local-judge/scripts/final-2026/run-buildstorm-ladder.sh
```

本地 Docker Desktop 仅有约 31 GiB 内存时，同一时刻最多运行一个 8 GiB guest；
正式独立资源上的 RV/LA 才可并行。外层 compile/full timeout 必须大于官方脚本内
14400 秒，项目统一使用 16000 秒。

## 8. 完成条件

只有 RV 与 LA 同时出现：

- `BUILDSTORM_TOOLCHAIN ok`
- `BUILDSTORM_MINIBUILD ok`
- `BUILDSTORM_COMPILE mode=multi ok=true`
- `[SINGLE] END ... exit=0`

并由原版官方 judge 生成新鲜分数后，本文才更新为“BuildStorm 已完成”。
