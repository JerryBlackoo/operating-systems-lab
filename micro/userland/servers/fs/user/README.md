# FS 服务（U-mode）

`userland/servers/fs/user/fs_main.c` 是独立的 U-mode FS 服务。**A1 之后它自带
ext4 与 vfs**：内核不再解析路径，也不再链接这两个模块。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `fs_path[]` / `fs_path_len` | `fs_main.c` | 客户端分块设置的目标路径（`SVC_MAX_PATH = 255`） |
| `fs_open_fd` / `fs_fd_valid` | `fs_main.c` | 缓存的 fd；`TAG_FS_PATH` 变更时关闭（A29） |
| `fs_prefetch[4096]` / `fs_prefetch_base` / `fs_prefetch_len` / `fs_prefetch_valid` | `fs_main.c` | 服务层读预取缓冲（A30） |
| `fs_ext4` | `fs_main.c` | 唯一的 `struct ext4` 实例 |
| `fs_vfs` / `fs_vfs_port` | `fs_main.c` | 唯一的 `struct vfs` 与它到 ext4 的四个回调 |
| `fs_blk_io` | `fs_main.c` | `ext4_blk_io`，`read_block` 接到 `ABI_BLOCK_READ` |
| `struct sha256_ctx` + `sha256_*` | `fs_main.c` | 只读自检用的 SHA-256（不依赖库） |
| 自检输出 | `fs_main.c` | `[fs] motd sha256=` / `[fs] largefile sha256=` / `[ext4] read /largefile: N bytes` / `[fs] ENOENT` |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `kernel/syscall/user_backend` | 块访问与容量 | `ABI_BLOCK_INFO`（容量/块大小）、`ABI_BLOCK_READ`（读原始块）——都只允许 `SERVICE_SID_FS` |
| `kernel/storage` + `block` + `virtio_blk` | 底层 I/O | `ABI_BLOCK_READ` → `kernel_storage_block_read_user` → `storage_read_block` → `block_submit`/`dispatch` |
| `ext4` / `vfs` | 同镜像内的策略层 | 直接函数调用（不是 IPC）：`ext4_mount_blk`、`ext4_read_at`、`vfs_open`/`vfs_read_at`/`vfs_close` |
| `sm` | 服务注册 | 启动先 `TAG_SM_REGISTER`（名字 `"fs"`） |
| `init` | 启动握手 | 自检完成后 `TAG_READY`；init 等到它才 spawn shell（A31） |
| `shell` | 唯一客户端 | `TAG_FS_PATH`（分块路径）+ `TAG_FS_READ`（offset/max）→ `TAG_FS_READ_OK`（24 B 数据块） |
| `console` | 日志输出 | 自检与错误都经 `user_print` → `TAG_CONSOLE_WRITE` |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 纯转发壳 | `fs_main.c` 只把路径与 offset 转给内核的 `ABI_VFS_READ_AT`；真正的 ext4/vfs 在内核 | M5/M7 先让端到端链跑通 |
| 2. 内核侧挂载 | `kernel_storage_bringup` 在内核 mount ext4 并直接读 `/motd` 打印哈希 | 需要真实的 virtio→block→ext4 证据 |
| 3. **策略层迁入 U-mode（A1）** | `ext4.c` + `vfs.c` 编进 `fs.elf`；内核只留 `block`/`virtio_blk`；`ABI_VFS_READ_AT` 删除，新增 `ABI_BLOCK_READ`(27)/`ABI_BLOCK_INFO`(28) | 内核不应解释路径、遍历目录、解码 inode/extent。内核 `.text` 从 90.5 KB 降到 84.1 KB |
| 4. 自检下沉 | `fs_self_check()` 在 U-mode 读 `/motd` 与 `/largefile` 并打印 SHA-256 | M5 判别依赖这些行；改镜像后哈希必须变，这条断言现在由 U-mode 产生 |
| 5. 栈溢出修正（A1b） | `loader` 的用户栈从 1 页扩到 4 页 | ext4+vfs 的调用深度在 4 KiB 栈上溢出，表现为 mount 前缺页被拒、服务完全不输出 |
| 6. fd 复用（A29） | 缓存上次成功的 fd | 24 B 分块读让 200 KiB 变成 ~8500 次 `TAG_FS_READ`，每次重开等于重做全部路径解析 |
| 7. 服务层预取（A30） | `fs_prefetch[4096]`；命中时纯内存拷贝 | `vfs_read_at` 的固定成本与读多少字节无关。`cat /largefile` 从 40–60 秒降到约 12 秒 |
| 8. 短读语义修正 | 预取缓冲剩余不足一个请求量时**重新预取**，不返回短读 | 客户端把 `n < SVC_DATA_BYTES` 当 EOF，会把 200 KiB 截断在第一个窗口 |
| 9. 读错误不当 EOF | `vfs_read_at` 失败时打印 `[fs] read error: <path>` 并跳过哈希 | 静默当 EOF 会算出「看起来正常」的哈希，让 M5 判别假通过 |
| 10. 挂载失败区分 | `!fs_mounted` 时回 `ABI_CLOSED` 而不是 `ABI_BAD_ARG` | 客户端要能区分「服务在但文件系统没起来」与「自己传错参数」 |

## 启动参数

| 参数 | cap |
| --- | --- |
| `a0` | ready send cap（slot 1） |
| `a1` | receipt `0x2001` |
| `a2` | FS endpoint recv cap（slot 2） |
| `a3` | 本服务 `sid` |
| `a4` | SM register send cap（slot 3） |

启动顺序：`TAG_SM_REGISTER` 注册 `"fs"` → `ABI_BLOCK_INFO` 拿容量 →
`ext4_mount_blk` → `fs_self_check()` → `TAG_READY`。

**为什么自检必须在 ready 之前**：init 要等 fs 与 console 都 ready 才 spawn
shell（A31）。否则 shell 一启动就打印 `micro>` 并响应命令，而 fs 的自检输出
会插进 help 的整行中间，破坏输出并让 `make check` 间歇性失败。

## 路径与读取协议

路径先分块设置（`SVC_PATH_CHUNK = 16`）：

```text
TAG_FS_PATH (0x501)
words[0] = 路径总长（<= 255）
words[1] = 本块 offset
words[2..3] = 16 B 路径块
```

再按 offset 读取（`SVC_DATA_BYTES = 24`）：

```text
TAG_FS_READ (0x503)
words[0] = offset
words[1] = max（FS 内部截到 24）
```

回复：

| tag | words | 含义 |
| --- | --- | --- |
| `TAG_FS_PATH_OK` (0x502) | `[0]=总长` | 本块已收 |
| `TAG_FS_READ_OK` (0x504) | `[0]=本次字节数, [1..3]=24 B 数据` | 正常数据块 |
| `TAG_FS_ERROR` (0x505) | `[0]=负数 ABI 状态` | 含 `ABI_NOT_FOUND` |

服务端**自己挂 ext4**（A1，2026-11-25）：

```text
ABI_BLOCK_INFO  → 容量/块大小
ABI_BLOCK_READ  → 逐块读原始数据
        ↓
ext4_mount_blk + vfs（两者都链进本 ELF）
```

内核只提供块设备驱动，不解释路径、不遍历目录、不解码 inode/extent。
`ABI_BLOCK_READ` 与 `ABI_BLOCK_INFO` 都只允许 `SERVICE_SID_FS`。

启动时服务会做一次只读自检并打印真实哈希（M5 判别依赖这些行）：

```text
[fs-svc] ext4 mounted in U-mode
[fs] motd sha256=<hex>
[fs] largefile sha256=<hex>
[ext4] read /largefile: 204800 bytes
[fs] ENOENT
```

自检的两个负向保证：
- 读错误不会被当 EOF（打印 `[fs] read error: <path>` 并跳过哈希）；
- `/no-such-file` 若意外打开成功，会先关 fd 再打印
  `ENOENT CHECK FAILED`（不泄漏 fd 槽，也不静默通过）。

改镜像后 `motd`/`largefile` 的哈希必须变化——这条断言现在由 U-mode 侧产生，
内核不再参与（内核只打印 `[block] self-test ok` 证明驱动能读到块）。

没有共享页，文件内容按 24 B IPC 块返回。

**性能边界**：`ABI_WORDS = 4` 决定单条 IPC 消息只携带 24 B 数据，因此
`cat` 一个大文件需要 `size/24` 次往返（200 KiB ≈ 8500 次）。服务端做了两级
优化把实际耗时压到约 12 秒：

1. 缓存 fd（同一路径的连续读只 `vfs_open` 一次，见 `fs_open_fd`）；
2. 4 KiB 预取缓冲（`fs_prefetch`）：`vfs_read_at` 的固定成本与读多少字节
   无关，预取后后续 ~170 次小读是纯内存拷贝。

**踩过的坑**：缓冲剩余长度不足一个请求量时不能返回「短读」——客户端会把
`n < SVC_DATA_BYTES` 当成 EOF，把文件截断在第一个预取窗口。必须先重新预取。

**实测结论（勿再怀疑 IPC）**：50 次 `cat /motd` 只花 21 ms。按 ino 缓存
inode 的方案试过并回退：`ext4` 的 host demo 会用同一实例重新指向不同镜像
内容来验证损坏检测，按 ino 命中会掩盖损坏。

**边界**：FS 不做客户端鉴权，持有 FS endpoint send cap 的进程即可设置路径并
读取任意文件；路径状态与缓存的 fd 都是单客户端全局量，不支持并发多客户端
（新的 `TAG_FS_PATH` 会关闭上一个 fd）。
