# [M3] 服务化 IPC：init / SM / PM 在 U-mode 真正组合

**依赖**：M2  
**建议 Agent**：A3  
**波次**：4

## 背景

现在 U-mode 的 IPC 闭环是**取证用的特例**，`kernel/syscall/closure.c` 文件头自己写清了边界：

> 另加的 `svc_*` 是一条有限的 U-mode sm 取证路径，不是完整 SM 服务：U-SM 在 bare satp 下运行、与内核共享映射，预置执行 register/lookup 后才 `RECV`，grant 只是 fixture token，没有 `cap_root_install` 也没有再次发送。

而 userland 那三个服务全是**纯模型**：

| 服务 | 现状 |
| --- | --- |
| `userland/servers/sm` | `demo.c` 用 `(slot, generation)` 句柄模拟三个客户端的能力空间 |
| `userland/servers/pm` | `pm_spawn` 向"任务替身"申请 AS / cap / thread，`demo.c` 里是 `provider` 结构 |
| `userland/init` | `init_boot(port=...)` 由 `system_setup` 注入假端口，`DEMO_MODULE(init, false, ...)` 连 U-mode 都不是 |

所以"微内核靠用户态服务组合"这件事，目前**没有任何一行真实代码在证明**。

## 教材对照

| 王道 | 汤小丹 |
| --- | --- |
| 2.1 进程的概念与状态 | 第 2 章 进程管理 |
| 2.3 进程同步（同步 rendezvous） | 第 3 章 进程同步与互斥 |
| 2.1 进程通信（消息传递） | 3.4 消息传递 |

## 目标

1. **init 真正在 U-mode 且从 initrd 加载**：`userland/init/` 的 `_start`（M2 建立的路径）负责：
   - 从初始能力里拿到 SM 的 endpoint cap；
   - 向 SM 注册自己并查询 `fs` 服务；
   - 按清单 `spawn`（经 PM）拉起 `fs` 与 `console` 两个服务；
   - 等待各自的 ready 回执，全部就绪后进入自己的主循环（或 `THREAD_EXIT` 并把控制权交给 shell，视 M7 决定）。
2. **SM 是真实服务进程**：`sm.c` 的模型不动（它的不变量已写好），但换成在**自己的 AS** 里运行，句柄来自 capability 而不是 `(slot, gen)` 模拟。`sm_port` 的六个操作由真实 capability 调用实现。
3. **PM 是真实服务进程**：`pm.c` 的 `pm_spawn` 把三个 provider 换成真实调用：
   - AS ← `as_create` + 加载器（M2）；
   - caps ← `cap_root_install` / cap 派生；
   - thread ← `thread_create` + `sched` 放行。
   `pm_on_exit` 接到真实 `THREAD_EXIT` 路径。
4. **删除 fixture 取证路径**：`closure.c` 里 `SID_SVC_*`、`sm_fixture`、`reply_fixture`、`user_prog.S` 的 fixture token 全部移除；`kernel/syscall/qemu-unified.log` 保留为历史证据（已归档至 `docs/evidence/qemu-unified-legacy.log`），但 README 要标注"该路径已被 M3 取代，见新 log"。
5. **cap 检查是真的**：一次**越权**的 send 必须由 capability 模块拒绝（`ABI_DENIED`），且拒绝原因是"缺 right"而不是"句柄不存在"——这两者要能在日志里区分。

## 交付

- `userland/init/`（真实 `_start` + 服务清单）
- `userland/servers/sm/`（真实端口绑定 + 保持模型不变量）
- `userland/servers/pm/`（真实三 provider）
- `kernel/syscall/`（删除 fixture 路径；必要时补 syscall 号：见下）
- `shared/abi/` 若新增调用号，必须同步 `abi.h` 与 `userland/libs/runtime.c`
- `kernel/syscall/README.md`、`userland/servers/{sm,pm}/README.md` 的「已知边界」更新
- `docs/design/M3-service-ipc.md`、`docs/evidence/M3-<日期>.log`

## 验收

```bash
timeout 20s make -C micro run; echo "EXIT=$?"
# 期望（顺序可不同，但每条都要出现一次）：
#   [init] boot: caps=<n> plan=<m>
#   [sm] serving
#   [init] spawn fs ok pid=<n>
#   [init] spawn console ok pid=<n>
#   [init] ready
#   [cap] denied: missing right SEND     <- 越权路径
# 且 EXIT=124
```

反向验收：

```bash
rg -n "fixture" micro/kernel/syscall/ micro/userland/   # 期望：无匹配
```

## 非目标

- 不做服务重启 / 故障隔离 / 监督树（等 M7 之后）
- 不做动态服务发现（清单是编译期固定的）
- 不做 IPC 超时与死锁检测
- 不改 `ipc.c` 的 rendezvous 语义（sendq/recvq 至多一个非空）

## 已知风险

- **`ep_call` 是同步的**：`closure.c` 的 `backend_invoke` 对 `ABI_IPC_CALL` 直接 `ep_call(...)` 并在同一次 syscall 里等回复。如果服务端在同一 hart 上且没有别的线程可跑，会死锁。M3 必须让 PM/SM/init/fs 都在**可调度**的线程上，且 idle 存在（M1）。验收时要专门跑一次"服务端未启动就 call"的负向用例，期望阻塞而非 panic，随后服务端启动后被唤醒。
- **每进程 cap 空间容量**：`CAP_MAX_TASKS=8`、`CAP_MAX_SLOTS=64`。init + sm + pm + fs + console + shell = 6，加 idle 与内核线程可能超。开工第一步先算容量，不够就调大并在 README 写明代价。
- **`satp` 切换与 `tp`**：现有 demo 在 bare satp 下运行 U-SM。真实多 AS 后每次切换都要 `as_activate` + `sfence.vma`，且 `tp`（hart_local）必须在 `sscratch`/trap 路径里正确保存，别在切换时丢掉。

## 纪律

必读：王道 2.1、2.3；汤小丹第 2、3 章。  
冲突时：`kernel/ipc/README.md` 的 rendezvous 不变量与 `kernel/capability/README.md` 的派生/撤销语义为准。  
完成后：只提交本 Goal 范围 diff，附 `docs/evidence/M3-<日期>.log`（含越权拒绝与阻塞唤醒两条）。
