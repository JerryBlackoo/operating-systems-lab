# M5 块设备 / ext4 / VFS（进行中）

## 裁决

- **块设备**：`virtio-blk`（QEMU `-device virtio-blk-device` + `readonly=on` 盘镜像），不用 NVMe/PCIe。
- **镜像工具**：宿主机 `mke2fs -t ext4 -O extent -b 4096 -d <rootfs>`，封装为 `tools/mkext4image.py`（含 `/motd` 与 200KiB `largefile`）。
- **常驻路径**：`kernel/storage/storage_bringup.c` 在 M3 init 拉起前完成探测与挂载冒烟（非 U-mode fs 服务终极形态；M7 再接 shell IPC）。

## 已接通

| 层 | 实现 |
| --- | --- |
| 镜像 | `make fs-image` → `build/micro-root.img` |
| QEMU | `SYSTEM_QEMUFLAGS` 附加 virtio-blk；`build/micro-root-run.img` 副本避免锁冲突 |
| 驱动 | `userland/servers/drivers/storage/virtio_blk/` MMIO 探测 + virtqueue |
| block | 仍用 `block.c` 模型；`storage_bringup` 填 `block_port` |
| ext4 | `ext4_mount_blk()` + `ext4_blk_io.read_block`；允许 `eh_depth=1`；忽略 `metadata_csum`；接受 `INCOMPAT_RECOVER`（只读） |
| VFS | 内核侧 `vfs_port` → ext4 |

## 当前缺口（证据 2026-11-24）

QEMU 日志已到：

- `[virtio-blk] mmio ready`
- `[block] dev0: capacity=8192 blocks`

随后 **`[block] completion timeout`**，`ext4 mount failed`——virtqueue 完成未在轮询路径上出现（疑似 DMA 描述符物理地址或 feature 协商，待修）。

## 验收命令（目标）

```bash
make -C micro fs-image
timeout 25s make -C micro run; echo "EXIT=$?"
# 期望：virtio → ext4 mounted → [fs] motd sha256=...
```

判别性：`mkext4image` 改 motd 后 sha256 应变；`[fs] ENOENT` 已预埋 `/no-such-file` 探测。