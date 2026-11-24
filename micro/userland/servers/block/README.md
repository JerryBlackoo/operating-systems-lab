# block server

块设备**共用**的请求策略：任何块设备（NVMe / virtio-blk / SD）都走这里，本目录不得出现任何具体硬件寄存器或协议细节。

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
2. 每个待完成请求只占一个位置。派发和完成事件串行处理，完成事件在派发状态登记之后处理。
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

本轮**没有**接通真实块设备。QEMU demo 使用内存盘替身和合成的完成项，
`[block] PASS` 只证明 FIFO、在途槽、完成匹配和失败投递模型；不代表 NVMe
队列、真实 DMA、IRQ 或 U-mode block 服务已经工作。剩余差距：真实队列/
中断接线、DMA 生命周期、设备复位与错误恢复。
