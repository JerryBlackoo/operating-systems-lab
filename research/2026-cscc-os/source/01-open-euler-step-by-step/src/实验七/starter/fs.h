#ifndef FS_H
#define FS_H

#include "disk.h"

// File system constants
#define MAX_FILENAME 28
#define MAX_FILE_SIZE (10 * 1024 * 1024)
#define MAX_OPEN_FILES 64
#define INODE_BLOCKS 16

// Block allocation
#define SUPERBLOCK_BLOCK 0
#define INODE_BITMAP_BLOCK 1
#define BLOCK_BITMAP_BLOCK 2
#define INODE_TABLE_BLOCK 3
#define INODE_TABLE_BLOCKS 32
#define DATA_BLOCK_START (INODE_TABLE_BLOCK + INODE_TABLE_BLOCKS)

// Inode structure
typedef struct {
    uint32_t inode_id;
    uint32_t size;
    uint32_t blocks[INODE_BLOCKS];
    uint16_t type;      // 0: free, 1: file, 2: directory
    uint16_t ref_count;
} Inode;

// Superblock structure
typedef struct {
    uint32_t magic;
    uint32_t total_blocks;
    uint32_t free_blocks;
    uint32_t inode_table_start;
    uint32_t data_block_start;
    uint32_t root_dir_inode;
} Superblock;

// File descriptor
typedef struct {
    int used;
    int inode_num;
    int offset;
    int flags;
} FileDescriptor;

// Directory entry
typedef struct {
    char filename[MAX_FILENAME];
    uint32_t inode_num;
} DirEntry;

typedef struct {
    Disk *disk;
    Superblock sb;
    FileDescriptor open_files[MAX_OPEN_FILES];
    int mounted;
} FileSystem;

FileSystem* fs_init(Disk *disk);
int fs_format(Disk *disk);
int fs_mount(FileSystem *fs);
int fs_create(FileSystem *fs, const char *filename, uint16_t type);
int fs_open(FileSystem *fs, const char *filename, int flags);
int fs_close(FileSystem *fs, int fd);
int fs_read(FileSystem *fs, int fd, void *buffer, int size);
int fs_write(FileSystem *fs, int fd, const void *buffer, int size);
int fs_delete(FileSystem *fs, const char *filename);
int fs_ls(FileSystem *fs);
void fs_sync(FileSystem *fs);

int alloc_inode(FileSystem *fs);
int alloc_block(FileSystem *fs);
void free_inode(FileSystem *fs, int inode_num);
void free_block(FileSystem *fs, int block_num);
Inode* read_inode(FileSystem *fs, int inode_num);
int write_inode(FileSystem *fs, int inode_num, Inode *inode);

int dir_add_entry(FileSystem *fs, int dir_inode_num, const char *filename, int inode_num);
int dir_remove_entry(FileSystem *fs, int dir_inode_num, const char *filename);
DirEntry* dir_read_entries(FileSystem *fs, int dir_inode_num, int *count);

#endif
