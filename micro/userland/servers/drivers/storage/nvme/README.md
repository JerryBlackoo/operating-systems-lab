# nvme 最小块设备驱动

驱动把一个逻辑块请求编码成 NVMe 命令，并根据完成队列交付结果。最小模型使用预置的单个 namespace 和一对 I/O 队列，至多保留一个在途请求。队列、doorbell 和 DMA 缓冲区由控制器替身提供。

## 最小模型

```text
Namespace = (nsid, lba_count, lba_bytes)
Driver    = (namespace, sq_tail, cq_head, cq_phase, in_flight, state)
Request   = (cid, op, lba, blocks, dma_buffer)

IDLE ──submit──> IN_FLIGHT ──匹配的完成项──> IDLE
                      └──完成项标识错误──> FAULTED ──复位确认──> IDLE
```

空队列的 `sq_tail` 和 `cq_head` 为 0，CQ 条目的 phase 初始化为 0，驱动最初期待 phase 1。 [NVMe Base Specification](https://nvmexpress.org/wp-content/uploads/NVM-Express-1_4c-2021.06.28-Ratified.pdf)

1. `submit(op, lba, blocks, buffer)` 接收读或写请求。校验非零块数、namespace 边界以及 DMA 方向和容量。本模型使用一个页对齐的 DMA 页，`PRP1` 指向该页，`PRP2` 保持 0，`NLB = blocks - 1`。因此一次提交的字节数不超过 4 KiB，更大的请求由 block 拆分。 [NVMe 命令格式](https://nvmexpress.org/wp-content/uploads/NVM-Express-1_4c-2021.06.28-Ratified.pdf)
2. 为请求保留命令 ID 和 DMA 引用，写入 SQ 条目。发布条目后，推进 `sq_tail` 并写 SQ Tail doorbell。
3. `poll()` 检查 `CQ[cq_head]` 的 phase。看到期望 phase 后读取完成项其余字段，核对 SQ ID 和命令 ID。成功匹配即消费一项并写 CQ Head doorbell，每绕回队首翻转一次期望 phase。 [NVMe PCIe 队列与 doorbell](https://www.nvmexpress.org/wp-content/uploads/NVMe-over-PCIe-Transport-Specification-1_0-2021.06.02-Ratified-1.pdf)
4. 交付完成状态并释放 DMA 引用。完成项标识异常，或没有在途请求时读到完成项，都进入 `FAULTED`。此时保留在途资源，等替身确认 DMA 已停止且队列已重建，再交付失败并回收。

## 不变量

1. 逻辑块范围满足 `lba < lba_count` 和 `0 < blocks <= lba_count - lba`，请求字节数在 DMA 页内且块数可编码。
2. 每个在途命令持有有效的 DMA 引用，一条合法完成只消费一次。读命令由设备写入缓冲区，写命令由设备读取缓冲区。
3. SQ 条目和在途记录先于提交通知发布。确认 CQ phase 并完成 DMA 读同步后，再读取完成内容和设备写入的数据。
4. 完成返回请求状态。成功时字节数为 `blocks * lba_bytes`，失败时模型返回零字节。

## 独立验证

使用深度为 2 的队列反复提交读取，注入成功或失败的 CQE，检查 phase 翻转、完成次数和 DMA 回收。再注入越界 LBA、非页对齐 DMA、错误方向和错误命令 ID / SQ ID，检查无效提交不会敲门铃，协议故障在复位确认后回收。

## QEMU demo

`nvme.c` 写入 64 字节 SQE 并消费 16 字节 CQE。控制器替身响应 doorbell，在内存中模拟 DMA 和完成投递。

```sh
make -C micro/userland/servers/drivers/storage/nvme run
```

从仓库根目录运行，成功时输出 `[nvme] PASS` 并退出 QEMU。

整机镜像通过自举清单选择本模块，清单中包含 `nvme` 时才会链接 `nvme.c`：

```sh
make -C micro run BOOT_MODULES=nvme
make -C micro run BOOT_MODULES=all
```

分层关系是 `vfs/ext4 → block → nvme`。驱动只把 block 下发的逻辑块请求翻译成 NVMe 命令，再把完成队列中的结果交回，不参与请求排队与调度。

## 职责

| 文件 | 职责 | Linux 对应 |
| --- | --- | --- |
| `nvme.c` | SQE/CQE 编解码、SQ/CQ 指针、doorbell、DMA 借用与故障恢复 | `drivers/nvme/host/pci.c` 的队列路径 |
| `nvme.h` | namespace、DMA 页、队列端口和驱动状态的最小结构 | `include/linux/nvme.h` |
| `demo.c` | 控制器替身：响应 doorbell、执行 DMA 并投递完成项 | — |

## 不归本目录

- 「请求先发哪一个、能否合并」→ `servers/block/`
- 「路径解析、fd 表、读偏移」→ `servers/vfs/`
- 「超级块、inode、extent、目录项」→ `servers/fs/ext4/`
- 「关中断、改页表、分配物理页」→ `kernel/`

## 边界判据

问：换成 virtio-blk 或 SD 卡，这段代码还需要吗？

- 需要 → 属于 `servers/block/` 的通用请求层
- 不需要（只有 NVMe 才有 SQE/CQE、PRP、doorbell）→ 本目录

## 本轮接线状态（勿过声称）

本轮**没有**接通真实 NVMe 控制器。demo 中的控制器、DMA 内存和完成队列都是
替身，`[nvme] PASS` 只证明 SQE/CQE 编解码、队列指针、doorbell 顺序和故障
恢复模型；没有 MMIO 探测、真实 IRQ、真实 DMA 或 U-mode 驱动服务。剩余差距：
PCIe/MMIO 初始化、队列内存分配与 IOMMU/PMP、真实中断与完成回收。
