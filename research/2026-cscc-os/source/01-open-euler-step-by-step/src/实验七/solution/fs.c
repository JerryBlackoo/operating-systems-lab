#include "fs.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAGIC 0xDEADBEEF

// Bitmap operations
static int test_bit(uint8_t *bitmap, int bit) {
    return bitmap[bit / 8] & (1 << (bit % 8));
}

static void set_bit(uint8_t *bitmap, int bit) {
    bitmap[bit / 8] |= (1 << (bit % 8));
}

static void clear_bit(uint8_t *bitmap, int bit) {
    bitmap[bit / 8] &= ~(1 << (bit % 8));
}

int fs_format(Disk *disk) {
    Superblock sb;
    sb.magic = MAGIC;
    sb.total_blocks = TOTAL_BLOCKS;
    sb.inode_table_start = INODE_TABLE_BLOCK;
    sb.data_block_start = DATA_BLOCK_START;
    sb.root_dir_inode = 0;

    // Clear inode bitmap
    uint8_t inode_bitmap[BLOCK_SIZE] = {0};
    set_bit(inode_bitmap, 0);

    // Clear block bitmap, reserve metadata blocks first
    uint8_t block_bitmap[BLOCK_SIZE] = {0};
    for (int i = 0; i < DATA_BLOCK_START; i++) {
        set_bit(block_bitmap, i);
    }

    // Create root directory inode
    Inode root_inode;
    memset(&root_inode, 0, sizeof(Inode));
    root_inode.inode_id = 0;
    root_inode.type = 2;  // directory
    root_inode.size = sizeof(DirEntry) * 2;
    root_inode.blocks[0] = DATA_BLOCK_START;
    set_bit(block_bitmap, DATA_BLOCK_START);

    // Now count free blocks correctly: everything after DATA_BLOCK_START that is NOT set
    uint32_t free_blk = 0;
    for (int i = DATA_BLOCK_START; i < TOTAL_BLOCKS; i++) {
        if (!test_bit(block_bitmap, i)) free_blk++;
    }
    sb.free_blocks = free_blk;

    // Write superblock into a 512-byte buffer (disk_write always writes BLOCK_SIZE bytes)
    uint8_t sb_buf[BLOCK_SIZE] = {0};
    memcpy(sb_buf, &sb, sizeof(sb));
    if (disk_write(disk, SUPERBLOCK_BLOCK, sb_buf) != 0) return -1;

    if (disk_write(disk, INODE_BITMAP_BLOCK, inode_bitmap) != 0) return -1;
    if (disk_write(disk, BLOCK_BITMAP_BLOCK, block_bitmap) != 0) return -1;

    // Write root inode into a 512-byte buffer
    uint8_t inode_buf[BLOCK_SIZE] = {0};
    memcpy(inode_buf, &root_inode, sizeof(root_inode));
    if (disk_write(disk, INODE_TABLE_BLOCK, inode_buf) != 0) return -1;

    // Create . and .. entries in root directory, write via 512-byte buffer
    DirEntry entries[2];
    memset(entries, 0, sizeof(entries));
    strcpy(entries[0].filename, ".");
    entries[0].inode_num = 0;
    strcpy(entries[1].filename, "..");
    entries[1].inode_num = 0;
    uint8_t dir_buf[BLOCK_SIZE] = {0};
    memcpy(dir_buf, entries, sizeof(entries));
    if (disk_write(disk, DATA_BLOCK_START, dir_buf) != 0) return -1;

    printf("Filesystem formatted successfully!\n");
    return 0;
}

FileSystem* fs_init(Disk *disk) {
    FileSystem *fs = (FileSystem *)malloc(sizeof(FileSystem));
    if (!fs) return NULL;
    memset(fs, 0, sizeof(FileSystem));
    fs->disk = disk;
    fs->mounted = 0;
    return fs;
}

int fs_mount(FileSystem *fs) {
    uint8_t sb_buf[BLOCK_SIZE];
    if (disk_read(fs->disk, SUPERBLOCK_BLOCK, sb_buf) != 0) return -1;
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

int alloc_inode(FileSystem *fs) {
    uint8_t bitmap[BLOCK_SIZE];
    if (disk_read(fs->disk, INODE_BITMAP_BLOCK, bitmap) != 0) return -1;

    int inode_num = -1;
    for (int i = 0; i < 1024; i++) {
        if (!test_bit(bitmap, i)) {
            inode_num = i;
            set_bit(bitmap, i);
            break;
        }
    }

    if (inode_num < 0) return -1;

    disk_write(fs->disk, INODE_BITMAP_BLOCK, bitmap);
    return inode_num;
}

int alloc_block(FileSystem *fs) {
    uint8_t bitmap[BLOCK_SIZE];
    if (disk_read(fs->disk, BLOCK_BITMAP_BLOCK, bitmap) != 0) return -1;

    int block_num = -1;
    for (int i = DATA_BLOCK_START; i < TOTAL_BLOCKS; i++) {
        if (!test_bit(bitmap, i)) {
            block_num = i;
            set_bit(bitmap, i);
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
    clear_bit(bitmap, inode_num);
    disk_write(fs->disk, INODE_BITMAP_BLOCK, bitmap);
}

void free_block(FileSystem *fs, int block_num) {
    uint8_t bitmap[BLOCK_SIZE];
    disk_read(fs->disk, BLOCK_BITMAP_BLOCK, bitmap);
    clear_bit(bitmap, block_num);
    disk_write(fs->disk, BLOCK_BITMAP_BLOCK, bitmap);
    fs->sb.free_blocks++;
}

Inode* read_inode(FileSystem *fs, int inode_num) {
    static Inode inode;
    int block = INODE_TABLE_BLOCK + (inode_num * sizeof(Inode)) / BLOCK_SIZE;
    int offset = (inode_num * sizeof(Inode)) % BLOCK_SIZE;

    uint8_t buffer[BLOCK_SIZE];
    if (disk_read(fs->disk, block, buffer) != 0) return NULL;

    memcpy(&inode, buffer + offset, sizeof(Inode));
    return &inode;
}

int write_inode(FileSystem *fs, int inode_num, Inode *inode) {
    int block = INODE_TABLE_BLOCK + (inode_num * sizeof(Inode)) / BLOCK_SIZE;
    int offset = (inode_num * sizeof(Inode)) % BLOCK_SIZE;

    uint8_t buffer[BLOCK_SIZE];
    if (disk_read(fs->disk, block, buffer) != 0) return -1;

    memcpy(buffer + offset, inode, sizeof(Inode));
    return disk_write(fs->disk, block, buffer);
}

int fs_create(FileSystem *fs, const char *filename, uint16_t type) {
    if (!fs->mounted) return -1;

    // Check if file already exists in root directory
    int count;
    DirEntry *entries = dir_read_entries(fs, 0, &count);
    if (entries) {
        for (int i = 0; i < count; i++) {
            if (strcmp(entries[i].filename, filename) == 0) {
                return -1;  // File exists
            }
        }
    }

    // Allocate inode
    int inode_num = alloc_inode(fs);
    if (inode_num < 0) return -1;

    // Initialize inode
    Inode inode;
    memset(&inode, 0, sizeof(Inode));
    inode.inode_id = inode_num;
    inode.type = type;
    inode.size = 0;
    write_inode(fs, inode_num, &inode);

    // Add directory entry
    if (dir_add_entry(fs, 0, filename, inode_num) != 0) {
        free_inode(fs, inode_num);
        return -1;
    }

    // Automatically open the newly created file and return fd (POSIX O_CREAT behavior)
    int fd = -1;
    for (int i = 0; i < MAX_OPEN_FILES; i++) {
        if (!fs->open_files[i].used) {
            fd = i;
            break;
        }
    }
    if (fd < 0) return inode_num;  // no free fd, at least return inode

    fs->open_files[fd].used = 1;
    fs->open_files[fd].inode_num = inode_num;
    fs->open_files[fd].offset = 0;
    fs->open_files[fd].flags = 0;
    return fd;
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

    // Find free file descriptor
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

    int file_size = inode->size;
    if (file->offset >= file_size) return 0;

    if (file->offset + size > file_size) {
        size = file_size - file->offset;
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
        if (to_read > size - total_read) {
            to_read = size - total_read;
        }

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
        if (to_write > size - total_written) {
            to_write = size - total_written;
        }

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

    // Check if file is open
    for (int i = 0; i < MAX_OPEN_FILES; i++) {
        if (fs->open_files[i].used && fs->open_files[i].inode_num == inode_num) {
            return -1;  // File is open
        }
    }

    // Free inode and data blocks
    Inode *inode = read_inode(fs, inode_num);
    if (inode) {
        for (int i = 0; i < INODE_BLOCKS; i++) {
            if (inode->blocks[i] != 0) {
                free_block(fs, inode->blocks[i]);
            }
        }
    }
    free_inode(fs, inode_num);

    // Remove directory entry
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
    // entries is a static array from dir_read_entries, must not be freed
    return 0;
}

void fs_sync(FileSystem *fs) {
    if (fs->mounted) {
        uint8_t sb_buf[BLOCK_SIZE] = {0};
        memcpy(sb_buf, &fs->sb, sizeof(fs->sb));
        disk_write(fs->disk, SUPERBLOCK_BLOCK, sb_buf);
    }
}
