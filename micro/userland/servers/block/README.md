# block server

块设备**共用**的请求策略：任何块设备（NVMe / virtio-blk / SD）都走这里，本目录不得出现任何具体硬件寄存器或协议细节。

**A1 之后**本模块仍在内核里（与 `virtio_blk` 一起），ext4/vfs 已迁到 U-mode。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct block_service` | `block.h` | 服务实例：`port`（驱动缝）、`last_id`、`devices[BLOCK_DEVICES=2]` |
| `struct block_device` | `block.h` | 单设备状态：`capacity`（逻辑块数）、`logical_bytes`、`pending[BLOCK_PENDING=4]` 环形队列、`in_flight`、`head`/`len` |
| `struct block_request` | `block.h` | 一次请求：`id`、`caller`、`dev`、`op`、`state`、`lba`/`blocks`、`bytes`、`buffer` |
| `struct block_buffer` | `block.h` | 数据缓冲描述：`data`、`size`、`readable`/`writable`/`borrowed` |
| `struct block_port` | `block.h` | **唯一的驱动缝**：`submit`（复制请求后立即返回）+ `done`（完成回调） |
| `enum block_state` | `block.h` | `BLOCK_QUEUED` / `BLOCK_IN_FLIGHT` / `BLOCK_DONE` |
| `enum block_status` | `block.h` | `BLOCK_OK` / `BLOCK_BAD_ARG` / `BLOCK_BUSY`（队首原地保留）/ `BLOCK_IO_ERROR` / `BLOCK_EMPTY` |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `drivers/storage/virtio_blk` | resident 的唯一驱动 | `virtio_blk_init` 设 `svc->port`；`driver_submit` / `driver_done` 在 `kernel/storage/storage_bringup.c` |
| `drivers/storage/nvme` | 另一个驱动（模型） | 两者都实现同一个 `block_port`；`block` 不知道区别 |
| `kernel/storage/storage_bringup` | 宿主与调用方 | `block_submit` / `block_dispatch` / `virtio_blk_poll` 的轮询循环；DMA 帧的分配与回收 |
| `ext4` | **曾经的直接调用方** | A1 前 ext4 在内核里直接 `block_submit`；现在 ext4 在 U-mode，经 `ABI_BLOCK_READ` 回到这里 |
| `kernel/syscall/user_backend` | ABI 落地 | `ABI_BLOCK_READ` → `kernel_storage_block_read_user` → `storage_read_block` → `block_submit`/`dispatch` |
| 单模块 demo | 内存盘替身 | `block` host demo 用合成驱动器验证 FIFO、BUSY、完成匹配、容量边界 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 单设备无队列 | 一次只有一个请求 | 先证明 submit → dispatch → complete 三步能对上 |
| 2. 有界 FIFO | `pending[4]` 环形队列 + `in_flight` 单槽 | 需要排队；容量必须有界，第 5 个请求返回 `BLOCK_BUSY` |
| 3. `BLOCK_BUSY` 的语义 | 驱动返回 BUSY 时队首**原地保留**，不移入在途槽 | 驱动可能暂时无法接收；丢掉队首会让上层看到「请求消失」 |
| 4. 完成按 id 匹配 | `block_complete(dev, id, status)` 只接受在途请求的 id | 乱序完成或重复完成必须被拒绝，否则缓冲会被写两次 |
| 5. 端口抽象 | `struct block_port` 的 `submit`/`done` | 让 NVMe 与 virtio-blk 共用同一套排队逻辑；`block` 里没有任何 MMIO |
| 6. 缓冲区不拷贝 | `block_buffer` 只存指针；调用者保证生命周期 | 4 KiB 块拷贝开销大；代价是请求在队列里时缓冲不能是栈变量（`storage_bringup.c` 里的 `g_io_buffer` 等静态描述符就是为此） |
| 7. 诊断用例 | `storage_busy_discriminate()`：填满队列再提交一个，断言 `BLOCK_BUSY` | 需要一个能证明「队列真的会拒绝」的可观测证据 |
| 8. 内核侧保留（A1） | ext4/vfs 迁走后 `block` 仍在内核 | 它是驱动策略而非文件系统策略；把它也迁到 U-mode 需要把 virtio MMIO 与中断/轮询一起搬，属下一步 |

## 最小模型

每个设备有一条有界 FIFO 和一个在途请求槽。块服务按顺序把读写请求交给驱动替身，再把完成结果交还原调用者。

```text
Device = (logical_block_bytes, capacity_blocks, pending, in_flight)
Request = (id, caller, dev_id, op, lba, nblocks, buffer_ref)

QUEUED ──驱动接收──> IN_FLIGHT ──匹配完成──> DONE
   └──驱动拒绝且返回错误───────────────────> DONE
```

| 操作 | 迁移 |
| --- | --- |
| `submit(dev, op, lba, nblocks, buf)` | 校验设备、范围和缓冲区。队列有空位时分配请求 ID，持有缓冲区引用并入队 |
| `dispatch(dev)` | 在途槽空闲时尝试提交队首。驱动接收后移入在途槽，暂忙时保留队首等下次派发，永久拒绝时交付错误 |
| `complete(dev, id, status)` | 核对在途请求，清理在途槽，交付完成结果并归还缓冲区 |

模型中 `lba` 对应接口里的 `sector`，与 `nblocks` 都以设备的逻辑块为单位。请求大小为 `nblocks * logical_block_bytes`。读盘要求设备能写缓冲区，写盘要求设备能读缓冲区。

## 不变量

1. 请求满足 `0 <= lba < capacity_blocks` 和 `0 < nblocks <= capacity_blocks - lba`。字节数乘积可表示，且落在缓冲区容量内。
2. 每个待完成请求只占一个位置。派发和完成事件串行处理：`dispatch` 在调用驱动 `submit` 之前先把队首请求置为 `IN_FLIGHT` 并登记到设备的在途槽，因此 submit 回调在 IN_FLIGHT 登记之后调用，驱动可以在同一次 submit 内完成该请求；完成事件必须在这次在途登记之后处理。驱动返回 `BUSY` 时必须回滚该登记，并保留队首和缓冲区借用。
3. 请求从入队到结束一直借用缓冲区并持有有效引用。请求 ID 唯一，只有匹配当前在途 ID 的完成事件可以交付结果。成功返回请求字节数，失败返回错误状态和零字节。

## 独立验证

用一个有 8 个逻辑块、每块 512 字节的内存盘。依次提交 LBA 2 的写和读，检查派发顺序、读回内容和完成计数。再注入 `lba=7, nblocks=2`、驱动暂忙、失败完成及重复完成，检查越界拒绝、队首保留和缓冲区回收。

## QEMU demo

`block.c` 实现 FIFO、在途槽和完成匹配。内存盘替身执行实际的字节复制，并投递成功或失败完成。

```sh
make -C micro/userland/servers/block run
```

从仓库根目录运行，成功时输出 `[block] PASS` 并退出 QEMU。

## 职责

| 文件（规划） | 职责 | Linux 对应 |
| --- | --- | --- |
| `bio.c` | 请求描述：设备、起始扇区、长度、内存页 | `block/bio.c` |
| `queue.c` | 每设备请求队列，入队 / 出队 / 完成回调 | `block/blk-mq.c` |
| `merge.c` | 相邻扇区请求合并 | `block/blk-merge.c` |
| `sched.c` | 调度：FCFS / SSTF / SCAN / deadline | `block/mq-deadline.c` |
| `bcache.c` | 块缓存（方案 A 时可暂不实现，见下） | `fs/buffer.c` |

## 不归本目录

- 文件系统格式（超级块、位图、inode、目录）→ `servers/fs/ext4/`
- 路径解析、fd 表、挂载点 → `servers/vfs/`
- 写 MMIO、收 IRQ、DMA 提交 → `servers/drivers/storage/*/`
- 关中断、改页表 → `kernel/`

## 边界判据

问：这个逻辑，换成另一种块设备还需要吗？

- 需要 → 本目录
- 不需要（只有 NVMe 才这样） → 驱动目录
- 不需要（只有 ext4 才这样） → `servers/fs/ext4/`

## 对外接口（IPC 消息，待 `shared/abi/` 定稿）

请求：`{ op, dev_id, sector, nblocks, buf_cap, flags }`
完成：`{ status, bytes, buf_handle }`

`buf_handle` 指向共享内存或 memory object，避免整块数据走消息拷贝。

## 缓存归属（两方案）

| 方案 | 块缓存放哪 | 代价 |
| --- | --- | --- |
| A（教学起步） | 各文件系统内部，本目录只做队列与调度 | 多文件系统时缓存不共享，可能不一致 |
| B（正规） | 本目录统一持有，fs 通过 IPC 取块 | 每次取块一次 IPC 往返，但全局一致 |

当前建议 A；切 B 时把 `bcache.c` 填上并把 `fs/ext4/` 的 `getblk` 改成 IPC。

## 本轮接线状态（勿过声称）

| 路径 | 块设备后端 | 说明 |
| --- | --- | --- |
| `BOOT_MODULES=capability`（默认） | **virtio-blk** + `storage_bringup` | 内核内 `block_dispatch` + PLIC；`tools/m5_discriminate.sh` 含队列满 `BLOCK_BUSY` 判别 |
| 单模块 `block` demo | 内存盘替身 | `[block] PASS` 只证明 FIFO/在途槽/完成匹配模型 |

未做：独立 U-mode block 服务、NVMe 真 MMIO、写路径、多设备热插拔。
