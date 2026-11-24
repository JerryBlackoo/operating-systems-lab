#ifndef SHARED_ABI_SYSCALL_H
#define SHARED_ABI_SYSCALL_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* 系统调用号。删除的编号保留，不复用。 */
enum {
    OS_SYS_NOP = 0x0000,
    OS_SYS_WRITE = 0x0001,
    OS_SYS_READ = 0x0002,
    OS_SYS_EXIT = 0x0003,
    OS_SYS_GETPID = 0x0004,
    OS_SYS_YIELD = 0x0005,
    OS_SYS_FORK = 0x0006,
    OS_SYS_EXEC = 0x0007,
    OS_SYS_SBRK = 0x0008,
    OS_SYS_OPEN = 0x0009,
    OS_SYS_CLOSE = 0x000a,
    OS_SYS_PS = 0x000b,
    OS_SYS_MKDIR = 0x000c,
    OS_SYS_STAT = 0x000d,
    OS_SYS_GETC = 0x000e,
    OS_SYS_PUTS = 0x000f,
};

/* 统一错误码：负值表示失败。 */
enum {
    OS_EPERM = -1,
    OS_ENOENT = -2,
    OS_EIO = -3,
    OS_EBADF = -4,
    OS_ENOMEM = -5,
    OS_EEXIST = -6,
    OS_EINVAL = -7,
    OS_ENOSYS = -8,
};

/* open 标志。 */
enum {
    OS_O_RDONLY = 0x0000,
    OS_O_WRONLY = 0x0001,
    OS_O_RDWR = 0x0002,
    OS_O_CREATE = 0x0100,
    OS_O_TRUNC = 0x0200,
};

/* 文件类型。 */
enum {
    OS_FT_REG = 1,
    OS_FT_DIR = 2,
    OS_FT_DEV = 3,
};

struct os_stat {
    uint32_t type;
    uint32_t size;
};

#ifdef __cplusplus
}
#endif

#endif /* SHARED_ABI_SYSCALL_H */
