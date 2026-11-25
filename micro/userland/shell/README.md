# shell（M7）

独立 U-mode AS 的交互 shell。它是系统里唯一的「用户」，其余服务都为它或
其它服务提供能力。

**启动顺序（A31 之后）**：init 先 spawn fs 与 console 并等两者 ready，**然后**
才 spawn shell。这个顺序是刚性的——shell 一启动就打印 `micro> ` 并响应命令，
若与 fs 的自检并行，fs 的 24 B 分块日志会插进 help 的整行中间。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `line[SHELL_LINE_MAX]` / `len` | `shell_main.c` | 当前命令行缓冲 |
| `console` / `fs` / `pm` 三个句柄 | `shell_main.c` | 启动时经 SM lookup 得到的服务端点；**不是**启动 cap |
| `con_write_raw` / `con_write_str` | `shell_main.c` | 把输出按 `SVC_DATA_BYTES = 24` 分块发给 console 服务 |
| `con_read_line` | `shell_main.c` | 发 `TAG_CONSOLE_READ` 取一行；不足一整块时用 `TAG_CONSOLE_MORE` 续传 |
| `fs_set_path` | `shell_main.c` | 把路径按 `SVC_PATH_CHUNK = 16` 分块发给 fs 服务 |
| `cmd_cat` / `cmd_ps` / `run_command` | `shell_main.c` | 四个内建命令与分派 |
| `append_u64` / `pack_chunk` / `unpack24` | `shell_main.c` | 数字格式化与 24/16 字节打包 |
| `sm_lookup` | `shell_main.c` | 循环重试的 SM 查询（服务可能还没注册） |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `console` 服务 | 唯一的 I/O 通道 | `TAG_CONSOLE_READ` / `MORE` / `WRITE`；shell 不碰 UART |
| `fs` 服务 | 唯一的文件访问 | `TAG_FS_PATH`（16 B 分块）+ `TAG_FS_READ`（offset/max）→ `TAG_FS_READ_OK`（24 B） |
| `pm` 服务 | 进程列表 | `TAG_PM_PS`（`words[0]=index`）→ `TAG_PM_PS_OK`（pid + 8B packed name + state + total） |
| `sm` | 服务发现 | `TAG_SM_LOOKUP` 三次（console / fs / pm），每次都用 `ABI_YIELD` 重试到成功 |
| `kernel` | 关机 | `ABI_POWEROFF`（仅 shell SID 可用） |
| `init` | 启动握手 | shell 回执 `TAG_READY(receipt = 0x2003)`；init 等到它才打印 `[init] ready` |

## 启动 ABI

PM `ABI_PM_SPAWN(SPAWN_SHELL)` 安装：

| 参数 | cap |
| --- | --- |
| `a0` | 向 init 回执 ready 的 send cap（slot 1） |
| `a1` | receipt `0x2003` |
| `a2` | SM register send cap（slot 3） |
| `a3` | 本服务 `sid`（`USER_SID_SHELL = 6`） |

shell 没有 recv 端点（它不是服务），也没有预置的 fs/console/pm 句柄——
这三个都靠 SM lookup 现取。

## 命令

| 命令 | 行为 |
| --- | --- |
| `help` | 打印 `commands: help cat ps echo poweroff`（**一行，不可被打断**） |
| `cat <path>` | `fs_set_path` → 循环 `TAG_FS_READ` 直到短读；输出 `----- cat <path> -----` / 数据 / `----- end cat -----` |
| `ps` | 逐 index 取 `TAG_PM_PS` 直到 `index >= total` |
| `echo <text>` | 原样回显 |
| `poweroff` | `ABI_POWEROFF`，内核打印 `[kmain] shutdown requested by shell` 后 `sbi_shutdown()` |

`cat` 的终止条件是 `n == 0` 或 `n < SVC_DATA_BYTES`。这个约定与 fs 服务的
预取逻辑必须匹配：fs 在缓冲剩余不足一个请求量时会**重新预取**而不是返回短读，
否则大文件会被截断在第一个窗口（A30 的坑）。

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. shell 直接调内核 | M7 早期 shell 自己发 `ABI_CONSOLE_READ/WRITE` 与 `ABI_VFS_READ_AT` | 少一跳；但内核必须内建「只有 shell 能用」的特权判断 |
| 2. 改为纯客户端 | 只持有 console/fs/pm 的 endpoint cap，全部经 IPC | 把特权收敛到服务主体；shell 变成可替换的普通程序 |
| 3. 分块协议 | 路径 16 B/块、数据 24 B/块、console 24 B/块 | `ABI_WORDS = 4` 让单条消息只有 24 B 载荷 |
| 4. `TAG_CONSOLE_MORE` 续传 | 一行可能超过 24 B | console 服务把整行缓存在 `pending_line`，按 offset 续传 |
| 5. SM lookup 重试 | `sm_lookup` 用 `ABI_YIELD` 循环 | shell 可能与目标服务并发启动，lookup 会先失败 |
| 6. `ps` 逐条查询 | `TAG_PM_PS(index)` 而不是一次性返回整表 | PM 的进程表在 U-mode，没有共享页；逐条取最省消息数 |
| 7. 与 fs 预取对齐 | `cat` 的 `n < 24` 判 EOF | fs 侧必须保证「只有真 EOF 才返回短读」，两侧语义必须一致 |
| 8. 启动顺序（A31） | init 分两阶段 spawn，shell 最后 | 否则 shell 的输出与 fs 自检日志交错，破坏 `help` 整行 |

## 已知边界

- 只有四个内建命令；无管道、重定向、通配、环境变量、历史、作业控制。
- 单行上限 `SHELL_LINE_MAX`（128），超出部分被内核 console 侧截断到 80。
- 不做引号/转义解析：`cat` 的路径取第一个空格之后的全部剩余内容。
- `cat` 大文件很慢：200 KiB ≈ 8500 次 `TAG_FS_READ` 往返，约 12 秒
  （`ABI_WORDS=4` 的固有代价；fs 侧两级优化后已从 40–60 秒降下来）。
- 路径状态与 fd 缓存都是 fs 服务的**单客户端全局量**，多 shell 会互相干扰。
