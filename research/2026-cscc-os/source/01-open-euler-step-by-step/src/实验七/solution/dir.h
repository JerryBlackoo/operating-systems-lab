#ifndef DIR_H
#define DIR_H

#include "fs.h"

int dir_add_entry(FileSystem *fs, int dir_inode_num, const char *filename, int inode_num);
int dir_remove_entry(FileSystem *fs, int dir_inode_num, const char *filename);
DirEntry* dir_read_entries(FileSystem *fs, int dir_inode_num, int *count);

#endif
