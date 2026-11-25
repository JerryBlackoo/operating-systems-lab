#ifndef MICRO_KERNEL_STORAGE_BRINGUP_H
#define MICRO_KERNEL_STORAGE_BRINGUP_H

#include <stddef.h>
#include <stdint.h>

struct as;

/*
 * 块设备驱动 bringup（virtio-blk + block 队列）。
 *
 * 这里**只有驱动**：ext4/vfs 已在 2026-11-25 迁到 U-mode `fs` 服务，
 * 内核不再链接文件系统策略，也不再解析路径。
 */
int kernel_storage_bringup(struct as *kas);

/*
 * 原始块读，仅 SERVICE_SID_FS。
 *
 * 把一个逻辑块读进调用者的用户缓冲区。内核不做任何文件系统解释：
 * 路径解析、目录遍历、inode/extent 解码全部留在 U-mode `fs` 里。
 * 成功时 *bytes_out 为实际读到的字节数。
 */
int64_t kernel_storage_block_read_user(uint8_t caller_sid, uint64_t lba,
                                       uintptr_t udst, size_t blocks,
                                       size_t *bytes_out);

/* 设备容量（块数），供 U-mode fs 校验超级块位置与 extent 边界。 */
uint64_t kernel_storage_capacity_blocks(void);

/* 逻辑块大小（字节）。 */
size_t kernel_storage_block_bytes(void);

#endif