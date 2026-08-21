#include "fs.h"
#include <string.h>

#define ENTRIES_PER_BLOCK ((size_t)(BLOCK_SIZE / sizeof(DirEntry)))

int dir_add_entry(FileSystem *fs, int dir_inode_num, const char *filename, int inode_num) {
    Inode *dir_inode = read_inode(fs, dir_inode_num);
    if (!dir_inode) return -1;

    size_t found = (size_t)-1;

    for (int i = 0; i < INODE_BLOCKS; i++) {
        if (dir_inode->blocks[i] == 0) {
            if (found == (size_t)-1) found = (size_t)i * ENTRIES_PER_BLOCK;
            continue;
        }
        DirEntry block_entries[BLOCK_SIZE / sizeof(DirEntry)];
        disk_read(fs->disk, dir_inode->blocks[i], block_entries);

        for (size_t j = 0; j < ENTRIES_PER_BLOCK; j++) {
            if (block_entries[j].inode_num != 0) {
                if (strcmp(block_entries[j].filename, filename) == 0) {
                    return -1;
                }
            } else if (found == (size_t)-1) {
                found = (size_t)i * ENTRIES_PER_BLOCK + j;
            }
        }
    }

    if (found == (size_t)-1) return -1;

    // Add new entry
    DirEntry new_entry;
    strncpy(new_entry.filename, filename, MAX_FILENAME - 1);
    new_entry.filename[MAX_FILENAME - 1] = '\0';
    new_entry.inode_num = inode_num;

    int block_idx = (int)(found / ENTRIES_PER_BLOCK);
    int entry_idx = (int)(found % ENTRIES_PER_BLOCK);

    if (dir_inode->blocks[block_idx] == 0) {
        int new_block = alloc_block(fs);
        if (new_block < 0) return -1;
        dir_inode->blocks[block_idx] = new_block;
    }

    DirEntry block_entries[BLOCK_SIZE / sizeof(DirEntry)];
    disk_read(fs->disk, dir_inode->blocks[block_idx], block_entries);
    block_entries[entry_idx] = new_entry;
    disk_write(fs->disk, dir_inode->blocks[block_idx], block_entries);

    write_inode(fs, dir_inode_num, dir_inode);
    return 0;
}

int dir_remove_entry(FileSystem *fs, int dir_inode_num, const char *filename) {
    Inode *dir_inode = read_inode(fs, dir_inode_num);
    if (!dir_inode) return -1;

    for (int i = 0; i < INODE_BLOCKS && dir_inode->blocks[i] != 0; i++) {
        DirEntry block_entries[BLOCK_SIZE / sizeof(DirEntry)];
        disk_read(fs->disk, dir_inode->blocks[i], block_entries);

        for (size_t j = 0; j < ENTRIES_PER_BLOCK; j++) {
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
            if (block_entries[j].inode_num != 0) {
                entries[*count] = block_entries[j];
                (*count)++;
            }
        }
    }
    return entries;
}
