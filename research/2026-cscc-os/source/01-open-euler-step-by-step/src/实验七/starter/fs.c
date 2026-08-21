#include "fs.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAGIC 0xDEADBEEF

// ========== 位图操作 ==========

static int test_bit(uint8_t *bitmap, int bit) {
    return bitmap[bit / 8] & (1 << (bit % 8));
}

static void set_bit(uint8_t *bitmap, int bit) {
    bitmap[bit / 8] |= (1 << (bit % 8));
}

static void clear_bit(uint8_t *bitmap, int bit) {
    bitmap[bit / 8] &= ~(1 << (bit % 8));
}

// ========== 核心函数 ==========

int fs_format(Disk *disk) {
    Superblock sb = {
        .magic = MAGIC,
        .total_blocks = TOTAL_BLOCKS,
        .inode_table_start = INODE_TABLE_BLOCK,
        .data_block_start = DATA_BLOCK_START,
        .root_dir_inode = 0
    };

    uint8_t inode_bitmap[BLOCK_SIZE] = {0};
    set_bit(inode_bitmap, 0);

    uint8_t block_bitmap[BLOCK_SIZE] = {0};
    for (int i = 0; i < DATA_BLOCK_START; i++) {
        set_bit(block_bitmap, i);
    }

    Inode root_inode = {0};
    root_inode.inode_id = 0;
    root_inode.type = 2;  // directory
    root_inode.size = sizeof(DirEntry) * 2;
    root_inode.blocks[0] = DATA_BLOCK_START;
    set_bit(block_bitmap, DATA_BLOCK_START);

    uint32_t free_blk = 0;
    for (int i = DATA_BLOCK_START; i < TOTAL_BLOCKS; i++) {
        if (!test_bit(block_bitmap, i)) free_blk++;
    }
    sb.free_blocks = free_blk;

    // Use 512-byte buffers because disk_write always writes BLOCK_SIZE bytes
    uint8_t sb_buf[BLOCK_SIZE] = {0};
    memcpy(sb_buf, &sb, sizeof(sb));
    disk_write(disk, SUPERBLOCK_BLOCK, sb_buf);
    disk_write(disk, INODE_BITMAP_BLOCK, inode_bitmap);
    disk_write(disk, BLOCK_BITMAP_BLOCK, block_bitmap);

    uint8_t inode_buf[BLOCK_SIZE] = {0};
    memcpy(inode_buf, &root_inode, sizeof(root_inode));
    disk_write(disk, INODE_TABLE_BLOCK, inode_buf);

    DirEntry entries[2];
    memset(entries, 0, sizeof(entries));
    strcpy(entries[0].filename, ".");
    entries[0].inode_num = 0;
    strcpy(entries[1].filename, "..");
    entries[1].inode_num = 0;
    uint8_t dir_buf[BLOCK_SIZE] = {0};
    memcpy(dir_buf, entries, sizeof(entries));
    disk_write(disk, DATA_BLOCK_START, dir_buf);

    printf("Filesystem formatted successfully!\n");
    return 0;
}

FileSystem* fs_init(Disk *disk) {
    FileSystem *fs = (FileSystem *)malloc(sizeof(FileSystem));
    memset(fs, 0, sizeof(FileSystem));
    fs->disk = disk;
    fs->mounted = 0;
    return fs;
}

int fs_mount(FileSystem *fs) {
    uint8_t sb_buf[BLOCK_SIZE];
    disk_read(fs->disk, SUPERBLOCK_BLOCK, sb_buf);
    Superblock sb;
    memcpy(&sb, sb_buf, sizeof(sb));
    if (sb.magic != MAGIC) {
        printf("Not a valid filesystem!\n");
        return -1;
    }
    fs->sb = sb;
    fs->mounted = 1;
    memset(fs->open_files, 0, sizeof(fs->open_files));
    printf("Filesystem mounted successfully!\n");
    return 0;
}

// ========== 块分配与释放 ==========

int alloc_inode(FileSystem *fs) {
    uint8_t bitmap[BLOCK_SIZE];
    disk_read(fs->disk, INODE_BITMAP_BLOCK, bitmap);

    int inode_num = -1;
    for (int i = 0; i < 1024; i++) {
        // TODO: 查找空闲inode (test_bit返回0表示空闲)
        if (!test_bit(bitmap, i)) {
            inode_num = i;
            set_bit(bitmap, inode_num);
            break;
        }
    }
    if (inode_num < 0) return -1;

    disk_write(fs->disk, INODE_BITMAP_BLOCK, bitmap);
    return inode_num;
}

int alloc_block(FileSystem *fs) {
    uint8_t bitmap[BLOCK_SIZE];
    disk_read(fs->disk, BLOCK_BITMAP_BLOCK, bitmap);

    int block_num = -1;
    for (int i = DATA_BLOCK_START; i < TOTAL_BLOCKS; i++) {
        // TODO: 查找空闲数据块
        if (!test_bit(bitmap, i)) {
            block_num = i;
            set_bit(bitmap, block_num);
            break;
        }
    }
    if (block_num < 0) return -1;

    disk_write(fs->disk, BLOCK_BITMAP_BLOCK, bitmap);
    fs->sb.free_blocks--;
    return block_num;
}

void free_inode(FileSystem *fs, int inode_num) {
    uint8_t bitmap[BLOCK_SIZE];
    disk_read(fs->disk, INODE_BITMAP_BLOCK, bitmap);
    // TODO: 清除inode位
    clear_bit(bitmap, inode_num);
    disk_write(fs->disk, INODE_BITMAP_BLOCK, bitmap);
}

void free_block(FileSystem *fs, int block_num) {
    uint8_t bitmap[BLOCK_SIZE];
    disk_read(fs->disk, BLOCK_BITMAP_BLOCK, bitmap);
    // TODO: 清除块位
    clear_bit(bitmap, block_num);
    disk_write(fs->disk, BLOCK_BITMAP_BLOCK, bitmap);
    fs->sb.free_blocks++;
}

// ========== inode读写 ==========

Inode* read_inode(FileSystem *fs, int inode_num) {
    static Inode inode;
    int block = INODE_TABLE_BLOCK + (inode_num * sizeof(Inode)) / BLOCK_SIZE;
    int offset = (inode_num * sizeof(Inode)) % BLOCK_SIZE;

    uint8_t buffer[BLOCK_SIZE];
    disk_read(fs->disk, block, buffer);
    memcpy(&inode, buffer + offset, sizeof(Inode));
    return &inode;
}

int write_inode(FileSystem *fs, int inode_num, Inode *inode) {
    int block = INODE_TABLE_BLOCK + (inode_num * sizeof(Inode)) / BLOCK_SIZE;
    int offset = (inode_num * sizeof(Inode)) % BLOCK_SIZE;

    uint8_t buffer[BLOCK_SIZE];
    disk_read(fs->disk, block, buffer);
    memcpy(buffer + offset, inode, sizeof(Inode));
    return disk_write(fs->disk, block, buffer);
}

// ========== 文件操作 ==========

int fs_create(FileSystem *fs, const char *filename, uint16_t type) {
    if (!fs->mounted) return -1;

    int count;
    DirEntry *entries = dir_read_entries(fs, 0, &count);
    if (entries) {
        for (int i = 0; i < count; i++) {
            if (strcmp(entries[i].filename, filename) == 0) {
                return -1;
            }
        }
    }

    // TODO: 分配inode
    int inode_num = alloc_inode(fs);
    if (inode_num < 0) return -1;

    Inode inode = {0};
    inode.inode_id = inode_num;
    inode.type = type;
    write_inode(fs, inode_num, &inode);

    // TODO: 添加目录项
    if (dir_add_entry(fs, 0, filename, inode_num) != 0) {
        free_inode(fs, inode_num);
        return -1;
    }

    return inode_num;
}

int fs_open(FileSystem *fs, const char *filename, int flags) {
    if (!fs->mounted) return -1;

    int count;
    DirEntry *entries = dir_read_entries(fs, 0, &count);
    int inode_num = -1;
    if (entries) {
        for (int i = 0; i < count; i++) {
            if (strcmp(entries[i].filename, filename) == 0) {
                inode_num = entries[i].inode_num;
                break;
            }
        }
    }
    if (inode_num < 0) return -1;

    int fd = -1;
    for (int i = 0; i < MAX_OPEN_FILES; i++) {
        if (!fs->open_files[i].used) {
            fd = i;
            break;
        }
    }
    if (fd < 0) return -1;

    fs->open_files[fd].used = 1;
    fs->open_files[fd].inode_num = inode_num;
    fs->open_files[fd].offset = 0;
    fs->open_files[fd].flags = flags;
    return fd;
}

int fs_close(FileSystem *fs, int fd) {
    if (fd < 0 || fd >= MAX_OPEN_FILES) return -1;
    if (!fs->open_files[fd].used) return -1;
    fs->open_files[fd].used = 0;
    return 0;
}

int fs_read(FileSystem *fs, int fd, void *buffer, int size) {
    if (fd < 0 || fd >= MAX_OPEN_FILES || !fs->open_files[fd].used) return -1;

    FileDescriptor *file = &fs->open_files[fd];
    Inode *inode = read_inode(fs, file->inode_num);
    if (!inode) return -1;

    if (file->offset >= (int)inode->size) return 0;
    if (file->offset + size > (int)inode->size) {
        size = inode->size - file->offset;
    }

    int total_read = 0;
    int offset = file->offset;
    char *buf = (char *)buffer;

    while (total_read < size) {
        int block_idx = offset / BLOCK_SIZE;
        if (block_idx >= INODE_BLOCKS) break;
        int block_offset = offset % BLOCK_SIZE;
        int block_num = inode->blocks[block_idx];
        if (block_num == 0) break;

        uint8_t block_data[BLOCK_SIZE];
        disk_read(fs->disk, block_num, block_data);

        int to_read = BLOCK_SIZE - block_offset;
        if (to_read > size - total_read) to_read = size - total_read;

        memcpy(buf + total_read, block_data + block_offset, to_read);
        total_read += to_read;
        offset += to_read;
    }

    file->offset += total_read;
    return total_read;
}

int fs_write(FileSystem *fs, int fd, const void *buffer, int size) {
    if (fd < 0 || fd >= MAX_OPEN_FILES || !fs->open_files[fd].used) return -1;

    FileDescriptor *file = &fs->open_files[fd];
    Inode *inode = read_inode(fs, file->inode_num);
    if (!inode) return -1;

    int total_written = 0;
    int offset = file->offset;
    const char *buf = (const char *)buffer;

    while (total_written < size) {
        int block_idx = offset / BLOCK_SIZE;
        int block_offset = offset % BLOCK_SIZE;
        if (block_idx >= INODE_BLOCKS) break;
        int block_num;

        if (inode->blocks[block_idx] == 0) {
            block_num = alloc_block(fs);
            if (block_num < 0) break;
            inode->blocks[block_idx] = block_num;
        } else {
            block_num = inode->blocks[block_idx];
        }

        uint8_t block_data[BLOCK_SIZE];
        disk_read(fs->disk, block_num, block_data);

        int to_write = BLOCK_SIZE - block_offset;
        if (to_write > size - total_written) to_write = size - total_written;

        memcpy(block_data + block_offset, buf + total_written, to_write);
        disk_write(fs->disk, block_num, block_data);

        total_written += to_write;
        offset += to_write;
    }

    file->offset += total_written;
    if (file->offset > (int)inode->size) {
        inode->size = file->offset;
        write_inode(fs, file->inode_num, inode);
    }

    return total_written;
}

int fs_delete(FileSystem *fs, const char *filename) {
    if (!fs->mounted) return -1;

    int count;
    DirEntry *entries = dir_read_entries(fs, 0, &count);
    int inode_num = -1;
    if (entries) {
        for (int i = 0; i < count; i++) {
            if (strcmp(entries[i].filename, filename) == 0) {
                inode_num = entries[i].inode_num;
                break;
            }
        }
    }
    if (inode_num < 0) return -1;

    for (int i = 0; i < MAX_OPEN_FILES; i++) {
        if (fs->open_files[i].used && fs->open_files[i].inode_num == inode_num) {
            return -1;  // 文件打开中
        }
    }

    Inode *inode = read_inode(fs, inode_num);
    if (inode) {
        for (int i = 0; i < INODE_BLOCKS; i++) {
            // TODO: 释放数据块
            if (inode->blocks[i] != 0) {
                free_block(fs, inode->blocks[i]);
            }
        }
    }
    // TODO: 释放inode
    free_inode(fs, inode_num);

    return dir_remove_entry(fs, 0, filename);
}

int fs_ls(FileSystem *fs) {
    if (!fs->mounted) return -1;

    int count;
    DirEntry *entries = dir_read_entries(fs, 0, &count);
    if (!entries) return -1;

    printf("Directory listing:\n");
    for (int i = 0; i < count; i++) {
        if (entries[i].filename[0] == '.') continue;
        Inode *inode = read_inode(fs, entries[i].inode_num);
        const char *type = (inode && inode->type == 2) ? "<DIR>" : "<FILE>";
        printf("  %s  %d  %s\n", type, inode ? inode->size : 0, entries[i].filename);
    }
    return 0;
}

void fs_sync(FileSystem *fs) {
    if (fs->mounted) {
        uint8_t sb_buf[BLOCK_SIZE] = {0};
        memcpy(sb_buf, &fs->sb, sizeof(fs->sb));
        disk_write(fs->disk, SUPERBLOCK_BLOCK, sb_buf);
    }
}
