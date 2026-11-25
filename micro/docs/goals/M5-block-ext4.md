# [M5] 真实块设备、ext4 与 VFS 读通

**依赖**：M3、M4  
**建议 Agent**：A5  
**波次**：5

## 背景

存储栈的三层各自都有明确标注的替身边界（`d05227e docs: record VFS/block/NVMe/ext4/net stand-in boundaries`）：

| 模块 | README 原话 | 真实缺口 |
| --- | --- | --- |
| `userland/servers/block` | "本轮**没有**接通真实块设备…`[block] PASS` 只证明 FIFO、在途槽、完成匹配和失败投递模型；不代表 NVMe…" | 驱动仍是内存盘替身 + 合成完成项 |
| `userland/servers/fs/ext4` | "块读取由内存镜像替身提供" / "挂载不验证 `gdt_csum` / `metadata_csum`" | 只读、`eh_depth=0`、无块设备 |
| `userland/servers/vfs` | "`[vfs] PASS` 只证明路径解析、每客户端偏移和描述符所有权；不代表块设备、ext4 挂载或 U-mode 文件服务已经接通" | 三层互不连通 |
| `userland/servers/drivers/storage/nvme` | "队列、doorbell 和 DMA 缓冲区由控制器替身提供" | 无真实 MMIO / PCI |
| `userland/servers/net` | "链路替身交付和接收完整 IP 包" | 无 virtio-net（本 Goal 非目标） |

所以"读一个文件"这条路现在是三段互不相连的模型。

## 教材对照

| 王道 | 汤小丹 |
| --- | --- |
| 4.1 文件系统基础 | 第 8 章 文件管理 |
| 4.2 文件系统实现（目录、inode） | 8.3 目录与文件实现 |
| 4.3 磁盘组织与管理 | 8.4 磁盘存储空间管理 |
| 5.2 设备驱动程序 | 9.3 设备驱动程序 |
| 5.3 磁盘调度 | 9.4 磁盘调度 |

## 目标

1. **一块真实块设备**：二选一，在 design 文档裁决并说明理由：
   - **virtio-blk**（推荐）：`-drive file=...,if=none,id=d0 -device virtio-blk-device,drive=d0`。virtio-mmio 在 `0x10001000..0x10008000`，IRQ 1..8（`kernel/interrupt/README.md` 第 1 节实测表）。需要 virtqueue 描述符链、`QueueNotify`、`InterruptStatus`。
   - **NVMe**：需要 PCIe ECAM 枚举 + BAR 映射，工作量大得多。
   无论哪个，`block.c` 的 FIFO/在途槽/完成匹配**不改**（它的边界判据写在 README 第 62 行："换一种块设备还需要吗？需要 → 本目录"），只把"驱动替身"换成真实驱动。
2. **中断驱动完成**：完成走 M4 建好的外部中断路径，不再靠轮询或合成完成项。`block` 的在途槽由 `complete` 回调推进。
3. **ext4 读真实镜像**：`micro/tools/mkext4image.py`（或复用系统 `mke2fs -t ext4 -O extent -b 4096`，**在 design 文档记录实际用的工具与命令**）产出一个只读镜像，`mount` 后能：
   - 解析 superblock / 块组描述符 / 根目录 / `/motd` 的 inode；
   - 通过 extent 叶子找到数据块，经 `block` 读出字节；
   - **至少支持 `eh_depth=1`**（extent 树一层内部节点），否则稍大的文件读不出来；
   - 校验和：只读模型可以忽略 `metadata_csum`，但**必须在 README 明确写出"忽略校验和，因此无法检测镜像损坏"**。
4. **VFS 三层连通**：`vfs → ext4 → block → virtio-blk/nvme → 真实镜像文件`。
5. **`cat /motd` 端到端**：由 U-mode 的客户端（M7 的 shell 或一个测试 app）经 IPC 请求 fs 服务，fs 服务返回字节，字节哈希与镜像内 `/motd` 一致。

## 交付

- `userland/servers/drivers/storage/virtio_blk/`（新）或补全 `nvme/` 的真实后端
- `userland/servers/fs/ext4/`（块读取换成 block 服务客户端；extent depth 1）
- `userland/servers/{block,vfs}/`（只改接缝，不改模型）
- `micro/tools/mkext4image.py` / `mkfs` 脚本
- `micro/Makefile`（QEMU 参数加 `-drive`/`-device`）
- 三个模块 README 的「已知边界」相应条目删除或改写
- `docs/design/M5-block-ext4.md`、`docs/evidence/M5-<日期>.log`

## 验收

```bash
# 1. 生成镜像并核对宿主机侧期望值
make -C micro fs-image
sha256sum build/micro-root.img   # 记录
# 从镜像里用宿主工具取出 motd 的期望哈希（debugfs 或 python 解析）

# 2. 跑系统
timeout 25s make -C micro run; echo "EXIT=$?"
# 期望：
#   [virtio-blk] mmio ready  或  [nvme] ready
#   [block] dev0: capacity=<n> blocks
#   [ext4] mounted: block_size=4096 inodes=<n>
#   [ext4] read /motd: <len> bytes
#   [fs] motd sha256=<hex>
# 且 EXIT=124
```

判别性验收（**必须做**，否则等于只证明了"能读一个我硬编码的东西"）：

```bash
# 改动镜像里的 /motd 内容，重跑，哈希必须变
# 用一个跨 >=1 个 extent 的大文件（例如 200 KiB）读，字节数与哈希都对
# 读一个不存在的文件：期望 [fs] ENOENT，不是 panic、不是空数据
# 把 block 的在途槽占满，验证 busy 重试路径仍成立（block 模型不变量 2）
```

## 非目标

- **不写文件**：不做 ext4 写、日志、`jbd2`、目录扩展、位图分配
- 不做 `metadata_csum` 校验
- 不做 `eh_depth >= 2`
- 不做 mount 多个文件系统 / 挂载点管理
- 不做 net（virtio-net 单独另开 Goal）
- 不做磁盘调度（`mono/` 的 G12 有教学版；微内核里这属于 block 服务内部策略，等有真实多请求并发再说）

## 已知风险

- **DMA 与页表**：virtio 描述符里的 `addr` 是**物理地址**。`frame_ptr()` 给的是内核直接映射 VA，必须换成 `paddr` 再写进描述符。写错的表现是设备读到垃圾或挂死，且没有报错。
- **`sfence.vma` 与设备可见性**：写完描述符后需要 `fence`（`fence rw,rw` 或 `fence iorw,iorw`）再 `QueueNotify`，否则设备可能看到旧描述符。
- **镜像布局与 `block` 的块大小**：`block.c` 的 `logical_block_bytes` 是请求粒度，ext4 的 `block_size` 是文件系统粒度。两者都是 4096 时最省事，若非 4096 必须写清换算。
- **不要把 ext4 做成 VFS 的一部分**。`vfs` 只认 `lookup/read_at`，`ext4` 只认块，这条边界（`ext4/README.md` 第 72 行的判据"换一个文件系统这段代码还需要吗？"）不许破坏。

## 纪律

必读：王道 4.1–4.3、5.2、5.3；汤小丹第 8、9 章。  
冲突时：`block`/`ext4`/`vfs` 三个 README 的「边界判据」为权威，不许把一层逻辑挪到另一层。  
完成后：只提交本 Goal 范围 diff，附 `docs/evidence/M5-<日期>.log`（含改动前/后两份 motd 哈希）。
