# vfs 路径与文件描述符

VFS 将路径解析成文件标识，并为每个客户端维护一张文件描述符表和各自的读偏移。最小模型固定一个只读根文件系统，覆盖打开、读取和关闭三步。

**A1 之后**本模块与 `ext4` 一起编译进 U-mode `fs` 服务，不再链进内核镜像。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct vfs` | `vfs.h` | 挂载点：`root`（根 inode）、`port`（后端端口）、`clients[VFS_CLIENTS=4]` |
| `struct vfs_port` | `vfs.h` | **唯一的后端缝**：`lookup` / `retain` / `release` / `read_at` |
| `struct vfs_node` | `vfs.h` | 解析结果：`inode` + `type`（`VFS_DIRECTORY` / `VFS_REGULAR`） |
| `struct vfs_client` | `vfs.h` | 一个客户端：`id`、`open`（已打开计数）、`files[VFS_FILES=8]` |
| `struct vfs_file` | `vfs.h` | 一个 fd 槽：`inode`、`used` |
| `enum vfs_result` | `vfs.h` | `VFS_OK` / `VFS_BAD_PATH` / `VFS_NOT_FOUND` / `VFS_NOT_DIR` / `VFS_NOT_FILE` / `VFS_BAD_FD` / `VFS_NO_SPACE` / `VFS_IO_ERROR` |

注意 `struct vfs_file` **不含 offset**——当前实现在 `read_at` 里由调用方显式
传 offset，不做隐式游标。这与「最小模型」里的 `(inode, offset)` 描述有出入：
现在的契约是 `vfs_read_at(vfs, client, fd, offset, ...)`。

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `ext4` | 唯一的后端实现 | `fs_vfs_lookup` / `fs_vfs_retain` / `fs_vfs_release` / `fs_vfs_read_at`（在 `userland/servers/fs/user/fs_main.c`） |
| `fs` 服务 | 宿主 | 路径与 fd 状态都是 `fs_main.c` 的静态变量；`TAG_FS_PATH` 变更时关闭缓存的 fd |
| `kernel/memory` | 无直接耦合 | VFS 不知道页表；它只调用 `read_at` 拿字节 |
| 单模块 demo | 内存 FS 替身 | `vfs` host demo 用一个内存目录树验证路径/fd/偏移语义，不碰 ext4 |
| `shell` | 间接客户端 | shell 经 `TAG_FS_READ` 让 fs 服务读，fs 服务内部走 VFS |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 路径解析 + fd 表（内存替身） | `resolve()` 逐段 `lookup`；`vfs_open` 分配 fd 槽 | 先把「路径→inode」「fd→(client, inode)」两层映射做对 |
| 2. 客户端隔离 | `find_client` / `find_free_client`；每个 client 8 个 fd 槽 | 多客户端不能互相覆盖 fd；`open` 归零时整块 client 槽释放 |
| 3. 拒绝 `.` / `..` 与空段 | `resolve` 里 `len == 0` 或 `.`/`..` 直接 `BAD_PATH` | 简化模型不支持目录穿越，避免需要父指针 |
| 4. 后端端口化 | `vfs_port` 的四个回调 | VFS 不依赖 ext4；单模块 demo 用内存替身，resident 用真 ext4 |
| 5. 接真 ext4 | `fs_vfs_*` 四个回调把 ext4 的 `EXT4_*` 映射成 `VFS_*` | 把「路径策略」与「磁盘格式」分开 |
| 6. 进入 U-mode（A1） | `vfs.c` 与 `ext4.c` 编进 `fs.elf`；内核只留块驱动 | 内核不应解释路径；`ABI_VFS_READ_AT` 被删除，改为 `ABI_BLOCK_READ` |
| 7. fd 复用（A29） | `fs_main.c` 缓存上次成功的 fd，`TAG_FS_PATH` 变更时才关闭 | 客户端按 24 B 分块读，大文件有上万次 `TAG_FS_READ`；每次重开等于重做全部路径解析 |

## 最小模型

```text
VFS = (root_inode, backend, client_table)
FD  : (client, fd) ⇀ (inode, offset)

空槽 ──open──> 已打开(offset = 0) ──close──> 空槽
                  └──read 成功──> offset += 实际读取长度
```

- `open(client, path)` 从根按绝对路径的分量调用 `lookup(parent_inode, name)`。前缀分量必须是目录，末项必须是普通文件。成功后在该客户端的表中分配最小的空闲描述符，偏移从 0 开始。每个客户端最多 `VFS_FILES` 个描述符，最多同时记录 `VFS_CLIENTS` 个客户端。
- `read(client, fd, n)` 按 `(client, fd)` 查得文件和偏移，调用 `read_at(inode, offset, n)`，按返回的字节数推进偏移。到达文件末尾得到空数据。
- `close(client, fd)` 删除描述符记录，交还打开时取得的文件引用。客户端关闭最后一个描述符后，其记录被释放，描述符编号从头开始分配。

文件系统替身提供目录项与文件字节。文件系统模块按 inode 与偏移读数据。 [ext4 模块](../fs/ext4/README.md)

路径分量不能为空，不能超长，也不接受 `.` 与 `..`。同一路径被同一客户端打开两次时，两条记录互不共享偏移。客户端身份取自请求上下文，不由描述符携带。

## 不变量

1. 描述符由请求上下文中的客户端身份限定。一个客户端的每次成功打开各有一个初始为零的偏移，不同客户端的同号描述符互不相干。
2. 路径由非空普通名称组成，逐级查找。成功分配描述符时对应的文件已确定。
3. 短读按实际长度推进，读取失败时偏移取调用开始时的值；读取返回的字节数超过请求长度按 I/O 错误处理。
4. 打开是事务性的：路径解析、客户端槽位、描述符槽位和文件引用任一失败时，不留下描述符记录，也不持有引用。描述符表或客户端表耗尽返回 `VFS_NO_SPACE`。

## 独立验证

用内存目录提供 `/demo/hello.txt`，同一客户端先后打开两次，验证 `fd` 从 0 递增且偏移独立；换一个客户端再次打开，验证 `fd` 重新从 0 开始。再检查相对路径、空分量、`.` / `..`、超长分量和类型不匹配，非法、越界、跨客户端及重复关闭的描述符，零长度读取，分段读取，短读，EOF，读错误与超长返回后偏移不变，以及描述符和客户端槽位耗尽、`retain` 失败后的回滚。

## QEMU demo

`vfs.c` 实现路径逐级查找和每客户端描述符表。内存文件系统替身提供目录及文件内容，记录每个 inode 的引用数、读取次数，并注入读错误、超长返回和 `retain` 失败。

```sh
make -C micro/userland/servers/vfs run
```

从仓库根目录运行，成功时输出 `[vfs] PASS` 并退出 QEMU。全内核一起运行：

```sh
make -C micro BOOT_MODULES=all run
```

## 本轮接线状态（勿过声称）

| 路径 | 后端 | 说明 |
| --- | --- | --- |
| `BOOT_MODULES=capability` | ext4 on virtio | **A1（2026-11-25）后 vfs 与 ext4 都链在 U-mode `fs` 服务里**；服务用 `ABI_BLOCK_INFO`/`ABI_BLOCK_READ` 读原始块，自己挂载 |
| 单模块 `vfs` demo | 内存 FS 替身 | `[vfs] PASS` 只证明路径/fd/偏移语义 |

未做：多挂载点、写路径、VFS 缓存跨 fs 共享。shell `cat` 按 16 B/块传路径、
按 24 B/块收内容（经 IPC，无共享页；见 `docs/design/M7-integration.md`）。

内核里已不再有 `vfs`：`make` 的 `C_SRCS` 只保留 `block` 与 `virtio_blk`，
`ext4.c`/`vfs.c` 由 `userland/servers/fs/user/Makefile` 编译进 `fs.elf`。
