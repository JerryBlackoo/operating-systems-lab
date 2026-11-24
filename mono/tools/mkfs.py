#!/usr/bin/env python3
"""生成教学 OS 的 RAM 盘镜像。

布局与 kernel/fs/fs.c 一致：
  block 0      superblock
  block 1      inode bitmap
  block 2      data bitmap
  block 3      inode table (64 * 40 bytes)
  block 4..    data blocks（/motd 占 motd_blocks 块，随后是根目录块）

根目录 inode 0，/motd 使用 inode 1 和若干数据块。
"""

import argparse
import os
import struct

BLOCK_SIZE = 4096
NBLOCKS = 256
NINODES = 64
INODE_BITMAP = 1
DATA_BITMAP = 2
INODE_TABLE = 3
DATA_START = 4
MAGIC = 0x4F534C42


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", nargs="?", default="build/ramdisk.img")
    parser.add_argument("--motd", default="userland/motd.txt")
    args = parser.parse_args()

    with open(args.motd, "rb") as f:
        motd = f.read()
    motd_blocks = (len(motd) + BLOCK_SIZE - 1) // BLOCK_SIZE
    if motd_blocks > 8:
        raise SystemExit("motd is too large for 8 direct blocks")
    # 根目录数据块紧跟 /motd 占用的块，避免大 motd 覆盖目录项。
    root_block = DATA_START + motd_blocks

    image = bytearray(NBLOCKS * BLOCK_SIZE)

    def block(n: int) -> int:
        return n * BLOCK_SIZE

    # superblock
    struct.pack_into(
        "<7I",
        image,
        block(0),
        MAGIC,
        NBLOCKS,
        NINODES,
        INODE_BITMAP,
        DATA_BITMAP,
        INODE_TABLE,
        DATA_START,
    )

    # inode bitmap: root + motd
    image[block(INODE_BITMAP)] = 0b11

    # data bitmap: /motd 数据块 + 紧随其后的根目录块
    for i in range(motd_blocks + 1):
        image[block(DATA_BITMAP) + i // 8] |= 1 << (i % 8)

    # inode table: type, nlink, size, direct[8]
    def inode(index: int, itype: int, size: int, direct: list[int]) -> None:
        offset = block(INODE_TABLE) + index * 40
        struct.pack_into("<HHI", image, offset, itype, 1, size)
        for i, value in enumerate(direct[:8]):
            struct.pack_into("<I", image, offset + 8 + i * 4, value)

    inode(0, 2, 32, [root_block])  # root directory, one 32-byte entry
    inode(1, 1, len(motd), list(range(DATA_START, DATA_START + motd_blocks)))

    # root directory entry: name[28] + ino
    entry = struct.pack("<28sI", b"motd", 1)
    image[block(root_block) : block(root_block) + len(entry)] = entry

    # /motd contents
    for i in range(motd_blocks):
        start = block(DATA_START + i)
        chunk = motd[i * BLOCK_SIZE : (i + 1) * BLOCK_SIZE]
        image[start : start + len(chunk)] = chunk

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    with open(args.output, "wb") as f:
        f.write(image)
    print(f"wrote {args.output}: {len(image)} bytes, motd={len(motd)} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
