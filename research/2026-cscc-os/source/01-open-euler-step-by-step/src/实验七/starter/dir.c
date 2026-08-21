#include "fs.h"
#include <string.h>

#define ENTRIES_PER_BLOCK ((size_t)(BLOCK_SIZE / sizeof(DirEntry)))

int dir_add_entry(FileSystem *fs, int dir_inode_num, const char *filename, int inode_num) {
    Inode *dir_inode = read_inode(fs, dir_inode_num);
    if (!dir_inode) return -1;

    // TODO: 查找或分配空闲目录项位置
    // Hint: 遍历dir_inode->blocks[]
    //       每个块可存储 ENTRIES_PER_BLOCK 个目录项

    DirEntry new_entry;
    strncpy(new_entry.filename, filename, MAX_FILENAME - 1);
    new_entry.filename[MAX_FILENAME - 1] = '\0';
    new_entry.inode_num = inode_num;

    // TODO: 写入目录项到磁盘

    return 0;
}

int dir_remove_entry(FileSystem *fs, int dir_inode_num, const char *filename) {
    Inode *dir_inode = read_inode(fs, dir_inode_num);
    if (!dir_inode) return -1;

    for (int i = 0; i < INODE_BLOCKS && dir_inode->blocks[i] != 0; i++) {
        DirEntry block_entries[BLOCK_SIZE / sizeof(DirEntry)];
        disk_read(fs->disk, dir_inode->blocks[i], block_entries);

        for (size_t j = 0; j < ENTRIES_PER_BLOCK; j++) {
            // TODO: 查找匹配的目录项并标记删除
            if (block_entries[j].inode_num != 0 &&
                strcmp(block_entries[j].filename, filename) == 0) {
                block_entries[j].inode_num = 0;
                block_entries[j].filename[0] = '\0';
                disk_write(fs->disk, dir_inode->blocks[i], block_entries);
                return 0;
            }
        }
    }
    return -1;
}

DirEntry* dir_read_entries(FileSystem *fs, int dir_inode_num, int *count) {
    Inode *dir_inode = read_inode(fs, dir_inode_num);
    if (!dir_inode) return NULL;

    static DirEntry entries[256];
    *count = 0;

    for (int i = 0; i < INODE_BLOCKS && dir_inode->blocks[i] != 0; i++) {
        DirEntry block_entries[BLOCK_SIZE / sizeof(DirEntry)];
        disk_read(fs->disk, dir_inode->blocks[i], block_entries);

        for (size_t j = 0; j < ENTRIES_PER_BLOCK; j++) {
            // TODO: 收集有效目录项
            if (block_entries[j].inode_num != 0) {
                entries[*count] = block_entries[j];
                (*count)++;
            }
        }
    }
    return entries;
}
