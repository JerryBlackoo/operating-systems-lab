# micro 验收证据索引

每个 Goal 完成后在此登记 `M<n>-YYYY-MM-DD.log`（完整命令、关键行、退出码）。**没有 log 不许在 goals/README 里标「已实现」。**

## 已有记录（基线前）

| 文件 | 说明 |
| --- | --- |
| [`../../kernel/boot/qemu-boot-capability.log`](../../kernel/boot/qemu-boot-capability.log) | 默认 capability 镜像 |
| [`../../kernel/boot/qemu-default-preempt.log`](../../kernel/boot/qemu-default-preempt.log) | timer 抢占探针 |
| [`../../kernel/boot/qemu-boot-umode.log`](../../kernel/boot/qemu-boot-umode.log) | `BOOT_MODULES=libs` U-mode ecall |
| [`qemu-unified-legacy.log`](qemu-unified-legacy.log) | syscall 双 AS + IPC + cap 拒绝（原 `kernel/syscall/qemu-unified.log`，已归档到本目录） |

## Goal 交付日志

| Goal | 日志与当前结论 |
| --- | --- |
| M0 | `M0-2026-11-24-summary.txt`、`M0-2026-11-24.log`；目标复核保留 `M0-2026-11-24-goal-recheck.log` 与 `M0-2026-11-24-shared-recheck.log` 的 EXIT=2 回归；补齐 ABI/运行时接线后，`M0-2026-11-24-goal-final.log` 记录 6 个 host 自测、13 个单模块镜像与 `BOOT_MODULES=all`，`make -C micro check` EXIT=0 |
| M1 | `M1-2026-11-24-current-resident.log`：当前集成镜像 5 次心跳、外层 `EXIT=124`；`M1-2026-11-24-current.log` 保留内层 timeout 被 make 转为 EXIT=2 的命令形状记录；`M1-check-regression.log` 保留全模块回归 |
| M2 | `M2-2026-11-24.log`：host 部分页/共享页/bss/负向/回滚 + QEMU 正向与 bad-machine 均通过；`M2-2026-11-24-current.log` 复核独立 ET_EXEC、内核无 `init_main` 与 loader host 自测，EXIT=0 |
| M3 | `M3-2026-11-24-partial.log`、`M3-2026-11-24-run.log`；中间/失败快照，不作为完成证据；`M3-2026-11-24-current-run.log` 记录当前 stub 服务与存储超时，M3 完整合同仍未达成 |
| M4 | `M4-2026-11-24-current-recheck.log`：真实 stdin 得到完整 abc、IRQ10 与 ext_irqs 增长，EXIT=0；`M4-2026-11-24-current.log` 保留镜像锁失败，`M4-2026-11-24.log` 保留早发输入失败 |
| M5 | `M5-2026-11-24-progress.log`：真实 legacy virtio-blk 到 VFS 的 `/motd` 正向读取及失败修复记录。**2026-11-25 复核**：`tools/m5_discriminate.sh` 已包含 block busy 判别断言（`[block] busy discriminate ok`），并校验默认/改写镜像两种 `/motd` 哈希与 200 KiB `/largefile` 全量哈希。实测 `[m5] PASS`（见 `/tmp/m5e.log` 与 `M5-2026-11-25-block-busy.log`）。遗留：`ext4`/`vfs`/`block`/`virtio_blk` 仍链在**内核**里，不是 U-mode 服务，见「已知架构偏离」 |
| M6 | **已完成**（2026-11-25）：`M6-2026-11-25-pager.log`、`M6-2026-11-25-pager-restore.log`、`M6-2026-11-25-discriminate.log`、`M6-2026-11-25-boot-stabilize.log`。U-mode `vm` 服务 + `kernel/pager` 缺页转 IPC + `heaptest`/`heaptest2` 双进程判别：同 VA `0x40020000` 落到不同物理帧（`0x8035b` vs `0x8035c`），越权 store 被拒（`deny reason=rights` → `[fault] rejected status=-4`），host 测试全绿。默认镜像已由内核拉起 vm/heaptest。**2026-11-25 补丁**：`ABI_VM_MAP` 另需 `cap_vm_can_map_as`（VM 持目标 AS 的 `CAP_RIGHT_MAP`；`loader`/`pager` 在首次缺页前 `cap_vm_ensure_as_map`） |
| M7 | `M7-2026-11-24.log`：独立 U-mode shell、真实 UART 行队列、FS/PM/SM endpoint、motd 哈希、ps 6 进程、poweroff；正向 exit 0，`--expect-nonzero` exit 1；`make check`/`shared check`/`BOOT_MODULES=all` 与 block/ext4/vfs/init/pm/sm 单模块均 exit 0。失败历史见 `M7-2026-11-24-preflight.log` |

## 已知架构偏离（2026-11-25 review 记录）

本树自称「不把策略堆进内核」，但当前默认镜像有下列偏离。它们**不是 bug**（功能与验收都成立），
但会让「微内核」这个说法失真。写 README／答辩时必须主动说明，不得只报 PASS。

| # | 偏离 | 证据 | 影响 |
| --- | --- | --- | --- |
| A1 | ~~**整个存储策略栈在 S-mode**~~ → **已闭合（2026-11-25）**：`ext4` 与 `vfs` 已从内核镜像移除，编译进 U-mode `fs` 服务（`userland/servers/fs/user/Makefile`）。内核只保留块设备驱动（`block` + `virtio_blk`），通过 **`ABI_BLOCK_READ`**（原始块读）与 **`ABI_BLOCK_INFO`**（容量/块大小）暴露访问。`ABI_VFS_READ_AT` 已删除（编号 13 留作空洞） | `kernel/storage/storage_bringup.c`（重写为纯驱动 + 块读）、`userland/servers/fs/user/fs_main.c`（自己 mount ext4、自己走 vfs）、`Makefile` | 内核 `.text` 从 90.5 KB 降到 84.1 KB，BSS 从 490112 降到 485072。路径解析、目录遍历、inode/extent 解码全在 U-mode。M5 判别的 motd/largefile 哈希改由 U-mode `fs` 打印，改镜像后哈希仍会变（`74bf28d9` → `86b469bf`），判别语义完整保留 |
| A1b | ~~U-mode 服务栈只有一页（4 KiB）~~ → **已补（2026-11-25）**：`map_user_stack` 映射 4 页（16 KiB） | `kernel/loader/loader.c` | A1 把 ext4+vfs 搬进 U-mode 后，挂载与路径解析的调用深度在单页栈上溢出（栈顶附近缺页 → VM 以 `reason=rights` 拒绝 → 进程死在 mount 前）。4 页覆盖当前所有 U-mode 服务的深调用，每进程仍只占 4 帧 |
| A2 | ~~vm 补页不校验目标 AS 的 cap~~ → **已补（2026-11-25）**：`cap_as` + `cap_holder_has_oid`；`ABI_VM_MAP` 要求 VM 持目标 AS 的 `MAP` cap | `cap_pm_install_as_grant` → `ABI_PM_SPAWN words[2]` → `ABI_VM_GRANT_AS` mint；**boot 直 spawn** 走 `kernel_init` `cap_vm_ensure_as_map`；`loader` 只 `cap_as_bind` | PM 路径不再 `cap_root_install` 直塞 VM；SM 不经手 AS cap |
| A3 | ~~SM 信任消息自称 sid~~ → **已补（2026-11-25）**：`ABI_REPLY_CALLER_SID` + `user_reply_caller_sid`；register/lookup 比对 `real_sid` 与 `claimed_sid` | `userland/servers/sm/user/sm_main.c`、`kernel/ipc/ipc.c` | SM 专有限权 syscall；其它服务若仍读 `words[]` 自称身份需逐点审计 |
| A4 | ~~异步 syscall 只有骨架~~ → **已补（2026-11-25）**：`ABI_IPC_CALL` 默认走 `ep_call_open`/`ep_call_deliver` + `syscall_wait_park` + `syscall_resume`，发起线程阻塞在 `trap` 内、由 `ep_reply` 写回调用帧；同实现保留 `ep_call` 同步回退（`make MICRO_SYSCALL_ASYNC_IPC=0`） | `ipc.c`、`user_backend.c:261`、`syscall.c`、`resident_trap.c` | 两条路径 `make check` + M7 demo 均 PASS，且均通过 `tools/shell_stress.py` 40 条命令长会话；`ipc_test` 覆盖 reply 槽位回收 |
| A5 | ~~异步 reply 对象泄漏~~ → **已补（2026-11-25）**：`ep_call_open` 的创建引用在 `ep_reply`、`ep_call_cancel_async`、`ep_destroy` 三条路径上归还 | `kernel/ipc/ipc.c`、`kernel/ipc/ipc_test.c:test_reply_open_slot_reuse` | 修复前 16 次 `IPC_CALL` 后槽位永久耗尽、shell 首条命令起全部静默；现在由 `make check` 的 `tools/shell_stress.py` 阶段守护 |
| A6 | ~~配置开关污染构建产物~~ → **已补（2026-11-25）**：`$(CONFIG_STAMP)` 记录 `ARCH/ASYNC_IPC/RESIDENT/MODULES`，内容变化即触发全量重编 | `Makefile` `$(CONFIG_STAMP)`、`clean` 删除该 stamp | 修复前切换 `MICRO_SYSCALL_ASYNC_IPC` 会复用旧 `.o`，产生「开关与镜像语义不符」的假 PASS／假 FAIL |
| A7 | ~~`sched_thread_discard` 可摘掉挂起在异步 syscall 上的线程~~ → **已补（2026-11-25）**：`async_parked` 标志位保护在途 trap 帧；强制终止路径先 `user_syscall_abandon_async` 撤销 reply | `kernel/sched/sched.c`、`kernel/syscall/syscall.c:syscall_wait_park` | 否则在途 `ep_reply` 会 `syscall_resume` 写进已被回收线程的 trap 帧 |
| A8 | ~~console 行队列在锁外消费~~ → **已补（2026-11-25）**：`console_read_line` 全程持 `sched_lock` | `kernel/console/console.c` | 原先隐含依赖「调用点 SIE=0」，与 UART IRQ 的 `flush_line` 存在竞争窗口 |
| A9 | ~~cap mint 失败残留不可用句柄~~ → **已补（2026-11-25）**：`cap_pm_mint_vm_map` / `cap_vm_ensure_as_map` 安装后复核，失败则 `cap_delete` 回滚 | `kernel/capability/cap_as.c` | 否则每次失败都在 VM CSpace 留一个无效 cap，最终耗尽槽位 |
| A10 | ~~`ABI_IPC_REPLY` 不校验回复者身份~~ → **已补（2026-11-25）**：`reply.owner_sid` 在消息交付接收者时绑定，`ep_reply_from()` 校验；U-mode `IPC_REPLY` 强制走带 sid 的入口 | `kernel/ipc/ipc.c`、`kernel/syscall/user_backend.c` | reply token 只是 bearer 整数、可被猜/伪造；不校验时任意进程都能抢答别人的请求或注入伪造应答 |
| A11 | ~~强制终止的线程会在 endpoint 队列里留下悬空引用~~ → **已补（2026-11-25）**：`ipc_release_thread()` 由 `thread_destroy` 回调，摘除 sendq/recvq 并清空 TCB 载荷 | `kernel/ipc/ipc.c`、`kernel/sched/sched.c` | 阻塞在 `ep_send` 的线程把消息体存在自己 TCB 里；不放的话接收者会读到已回收线程的内存 |
| A12 | ~~loader 的 launch 槽（含内核栈）在请求终止时被提前释放~~ → **已补（2026-11-25）**：改由 `thread_destroy` 钩子 `loader_release_launch_for_thread` 统一归还 | `kernel/loader/loader.c`、`kernel/sched/sched.c` | RUNNING 线程还在用自己的 kstack；提前释放会让下一次 spawn 复用同一块内存并踩坏它 |
| A13 | ~~`as_unmap` 不回收变空的中间页表页~~ → **已补（2026-11-25）**：解除映射后自底向上回收空表 | `kernel/memory/as.c`、`kernel/memory/memory_test.c:test_page_table_reclaim` | 旧实现只清叶子 PTE；反复 map/unmap 不同 VA 会把页表页永久吃掉，demand paging 长跑必然耗尽帧池 |
| A14 | ~~VM 的 region/驻留页槽位无释放入口~~ → **已补（2026-11-25）**：新增 `TAG_VM_RELEASE`/`TAG_VM_RELEASE_OK` 并在 resident `vm_main` 处理，调用 `vm_release` 撤销区间与驻留页 | `shared/abi/service.h`、`userland/servers/vm/user/vm_main.c` | `vm_release` 早已实现且有 host 用例，但**协议里没有释放消息**，resident VM 从不调用它；每次 reserve 永久占用一个 `VM_REGIONS` 槽，长期运行的进程在槽位耗尽后所有缺页都被拒 |
| A15 | ~~`irq_port.intr_restore` 与 `sched_unlock` 语义不一致~~（**非缺陷，已确认**）：PLIC 端口在 `flags==0` 时显式清 SIE，`sched_unlock` 则保持不动 | `kernel/arch/riscv/plic.c` | 两者都只在各自模块的短临界区内成对使用，不会互相嵌套破坏外层状态；记录在此避免后续误改 |
| A16 | ~~pager 缺页无超时~~ → **已补（2026-11-25）**：每个 pending 槽带 `clock_timer` 看门狗，`PAGER_TIMEOUT_NS`（200ms）到期把槽标成 `DONE(ABI_TIMED_OUT)` 并唤醒缺页线程，由 `reject_and_exit` 结束它 | `kernel/pager/pager.c`、`shared/abi/abi.h`（新增 `ABI_TIMED_OUT=-11`） | 修复前 VM 若卡住不调 `ABI_PAGER_DONE/FAIL`，缺页线程永远挂在 `pager_waitq` 上，既不响应也无法回收。1ms 实测能触发 `[fault] rejected ... status=-11`，200ms 下正常补页无误杀 |
| A17 | ~~VM 看不到进程退出，区间/驻留页槽位永不回收~~ → **已补（2026-11-25）**：新增 `TAG_VM_FORGET`（PM 发送）+ `vm_forget()`；PM 在 `ABI_PM_DISCARD` 成功后通知 | `shared/abi/service.h`、`userland/servers/vm/vm.c:vm_forget`、`userland/servers/pm/user/pm_main.c`、`kernel/boot/service_boot.c`（PM 新增 `SLOT_PM_VM`） | 此前只能靠客户端显式 `TAG_VM_RELEASE`；崩溃或被杀掉的进程会把自己用过的槽位永久占住，最终所有缺页被拒 |
| A18 | ~~U-mode 自检的 SHA-256 填充可能死循环~~ → **已补（2026-11-25）**：改为显式按块填充（补 `0x80` → 若越过 56 先刷块 → 补零到 56 → 写长度） | `userland/servers/fs/user/fs_main.c:sha256_finish_hex` | 原写法 `while (used != 56) update(0)` 在 `used` 落在 57..63 时要绕过一整个块才能回到 56，语义绕且脆弱。修复后 200 KiB 与短文件的哈希都与内核版本逐字节一致 |
| A19 | ~~`TAG_VM_FORGET` 不校验发送者身份~~ → **已补（2026-11-25）**：VM 用 `ABI_REPLY_CALLER_SID` 查真实 sid，仅接受 `USER_SID_PM`；该 ABI 从「仅 SM」放宽到「SM 或 VM」 | `userland/servers/vm/user/vm_main.c`、`kernel/syscall/user_backend.c` | heaptest/heaptest2 也持 VM 发送能力（M6 需要），不校验时它们能清掉任意进程的地址空间区间，让对方缺页全部失败 |
| A20 | ~~FS 自检把读错误当 EOF~~ → **已补（2026-11-25）**：`vfs_read_at` 失败时打印 `[fs] read error: <path>` 并跳过该文件哈希 | `userland/servers/fs/user/fs_main.c` | 原实现 `n=0` 静默当 EOF，会算出「看起来正常」的哈希让 M5 判别假通过 |
| A21 | ~~`TAG_VM_FORGET` 用阻塞 `send` 与注释矛盾~~ → **已补（2026-11-25）**：改用 `user_ipc_call`（VM 需要 reply token 才能查发送者身份） | `userland/servers/pm/user/pm_main.c` | `ABI_IPC_SEND` 是 rendezvous，VM 不在 recvq 时会把 PM 的 spawn 失败路径阻塞住 |
| A22 | ~~`service_proto.h` 与 `abi/service.h` 是两份独立副本~~ → **已补（2026-11-25）**：SID 与 tag 统一由 `shared/abi/service.h` 定义，`service_proto.h` 只引用它 | `shared/abi/service.h`、`userland/libs/service_proto.h` | 新增 tag 必须改两处，漏改会让 U-mode 发出的 tag 内核不认识；SID 也曾在两侧各有一份 |
| A23 | ~~`ABI_BLOCK_READ` 目标地址未防溢出~~ → **已补（2026-11-25）**：显式检查 `udst > UINTPTR_MAX - i*BLOCK_BYTES` | `kernel/storage/storage_bringup.c` | 当前 `blocks` 上限为 1 不会触发，但不应依赖那个常数 |
| A24 | ~~pager watchdog 的 `clock_timer_arm` 返回值被忽略~~ → **已补（2026-11-25）**：arm 失败时清槽并返回 `NULL`（缺页随即被拒），槽位选择时跳过残留 `RUNNING` 的定时器 | `kernel/pager/pager.c:pending_add` | `clock_timer_arm` 在「同一 hart 上该定时器上次回调仍 RUNNING」时返回 false；忽略它会让这次缺页没有超时保护，重新引入无界阻塞 |
| A25 | ~~`kernel_pm_spawn` 在失败路径前就消耗 pid~~ → **已补（2026-11-25）**：`spawn_next_pid++` 移到所有失败分支之后，并显式检查 `child == 0` | `kernel/boot/kernel_spawn.c` | 原实现先 `*pid_out = spawn_next_pid++` 再装 AS cap；cap 安装失败 abort 后 pid 已被吃掉，`ps` 出现跳号、PM 可能持有已 abort 线程的 pid |
| A26 | ~~PM 在 `words[2]==0` 时静默跳过 VM grant~~ → **已补（2026-11-25）**：分别打印 `no thread id` / `no AS cap, VM grant skipped` | `userland/servers/pm/user/pm_main.c` | 静默跳过会让该进程日后缺页以 `ABI_DENIED` 死掉却查不到原因 |
| A27 | ~~`words[1]==0` 时仍登记进程~~ → **已补（2026-11-25）**：thread id 为空时直接回 `TAG_PM_ERROR(ABI_NO_MEMORY)` 并 continue | `userland/servers/pm/user/pm_main.c` | A26 的日志分支漏了「不登记」这一步：thread id 为 0 的进程永远无法运行、也无法被 discard，是幽灵记录，`ps` 的 total 会偏大 |
| A28 | ~~FS 自检的 ENOENT 探针可能泄漏 fd~~ → **已补（2026-11-25）**：若 `/no-such-file` 意外打开成功，先 `vfs_close` 再报 `ENOENT CHECK FAILED` | `userland/servers/fs/user/fs_main.c` | 自检本身泄漏一个 fd 槽会让后续 `cat` 在 `VFS_FILES` 耗尽后失败；静默通过则失去负向断言的意义 |
| A29 | ~~`TAG_FS_READ` 每次都重新 `vfs_open`~~ → **已优化（2026-11-25）**：缓存上次成功的 fd，收到新 `TAG_FS_PATH` 时才关闭 | `userland/servers/fs/user/fs_main.c` | 读 200 KiB 要 ~8500 次 `TAG_FS_READ`，每次都重做完整路径解析（每层 `ext4_lookup` 读目录块）。复用 open 后同样次数的读只解析一次路径 |
| A30 | ~~FS 服务在大文件读取上逐块往返 ext4~~ → **已优化（2026-11-25）**：服务层加 4 KiB 预取缓冲，命中时纯内存拷贝 | `userland/servers/fs/user/fs_main.c`（`fs_prefetch`） | `vfs_read_at` 的固定成本（`load_inode` + extent 解析 + 块映射）与读多少字节无关，而客户端每次只要 24 B。预取后 `cat /largefile` 从 40–60 秒降到约 12 秒。实现时踩过一个坑：缓冲剩余不足一个请求量时不能返回「短读」，否则客户端会当成 EOF，把文件截断在第一个窗口 |
| A31 | ~~**U-mode `fs` 自检输出与 shell 输出交错**~~ → **已补（2026-11-25）**：`init` 改为分阶段 spawn —— 先拉 fs/console 并等它们 ready，再拉 shell | `userland/init/user/init_main.c` | 此前 shell 与 fs 并行启动，shell 一启动就打印 `micro>` 并响应命令，而 fs 的只读自检（读 /motd 与 /largefile 算 SHA-256）要花时间，其分块输出会插进 `commands: help cat ps echo poweroff` 中间把这行拆成两段。表现为 `make check`/stress **间歇性**失败（约 1/8 概率），用户看到的 help 输出也被破坏。修复后 10 次 `make check`、8 次 stress 全部通过 |

### 已知性能边界（2026-11-25 实测后更新）

`ABI_WORDS = 4` 让单次 IPC 消息只能携带 24 字节数据（1 word 存长度）。
`cat /largefile`（200 KiB）需要约 8500 次往返。

**实测定位**（不要猜是 IPC 慢）：
- 50 次 `cat /motd` 只消耗 21 tick（≈21 ms）→ **IPC 往返不是瓶颈**
- 真正开销在 `vfs_read_at` 的固定成本：`load_inode`（读 inode 表块 +
  解析 extent）+ 块映射，与读多少字节无关

**已做的两级优化**：
1. `A29` 缓存 fd（路径只解析一次）
2. `A30` 服务层 4 KiB 预取（后续 ~170 次小读变成纯内存拷贝）

效果：`cat /largefile` 从 40–60 秒降到约 **12 秒**。

**尝试过并回退**：给 ext4 加按 ino 的 inode 缓存。它让 `ext4` 的 host demo
失败——该 demo 故意用**同一个 `struct ext4` 实例**重新指向不同镜像内容来
验证「extent 损坏必须被检测」，按 ino 命中的缓存会掩盖损坏。安全版本需要
缓存 key 含镜像身份，成本高于收益。

**剩余可消除路径**（未做）：扩大消息载荷，或引入「fs 直接把数据写进
调用者 AS」的新 ABI（类似 `ABI_BLOCK_READ`）。属架构改动。

### 回归工具

| 工具 | 断言 |
| --- | --- |
| `tools/demo.sh` | M7 端到端：shell 提示符、help/cat/ps/echo/poweroff、motd 哈希 |
| `tools/demo.sh --expect-nonzero` | 负向：不存在的路径不得当成 motd 成功 |
| `tools/shell_smoke.py` | **交互输出完整性**：逐条命令的完整输出，以及 help 整行未被其它服务日志交错（A31 的守护） |
| `tools/shell_stress.py N` | 长会话：N 条命令后 IPC 资源不泄漏 |
| — | **PM discard 收尾（2026-11-25）**：`loader_terminate_spawn` → `sched_thread_terminate`；RUNNING 登记延迟退出、异步 park 先撤销 reply | `ABI_PM_DISCARD=26` | 原先「子线程已 RUNNING → `ABI_BUSY`」的尾巴已消除；只有 current/idle 才拒绝 |