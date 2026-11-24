#ifndef KERNEL_FS_FS_H
#define KERNEL_FS_FS_H

#include <stdint.h>

#define FS_MAGIC 0x4f534c42u /* "OSLB" */
#define FS_MAX_INODES 64
#define FS_MAX_FILES 32

struct fs_inode {
    uint16_t type;      /* 0 free, 1 file, 2 dir */
    uint16_t nlink;
    uint32_t size;
    uint32_t direct[8];
};

struct fs_dirent {
    char name[28];
    uint32_t ino;
};

struct file {
    int used;
    int refs;           /* fork 后父子进程共享同一个打开文件 */
    int ino;
    int dev;            /* 0 = FS, 1 = null, 2 = zero */
    uint32_t off;
    int flags;
};

void fs_init(void);
void fs_selftest(void);

int fs_lookup(const char *path);
int fs_create(const char *path, int type);
int fs_mkdir(const char *path);
int fs_read(int ino, uint32_t off, void *dst, uint32_t len);
int fs_write(int ino, uint32_t off, const void *src, uint32_t len);
int fs_truncate(int ino);
int fs_readdir(int ino, uint32_t index, struct fs_dirent *out);
int fs_is_dir(int ino);

struct file *fs_file_alloc(void);
struct file *fs_file_at(int index);
int fs_file_index(struct file *f);
void fs_file_dup(struct file *f);
void fs_file_free(struct file *f);

#endif
