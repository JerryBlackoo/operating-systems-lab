#ifndef USERLAND_LIB_H
#define USERLAND_LIB_H

#include "shared/abi/syscall.h"

static inline long os_syscall3(long nr, long a0, long a1, long a2)
{
#ifdef PLATFORM_ZYNQ
    register long r0 __asm__("r0") = a0;
    register long r1 __asm__("r1") = a1;
    register long r2 __asm__("r2") = a2;
    register long r7 __asm__("r7") = nr;

    __asm__ volatile("svc #0" : "+r"(r0) : "r"(r1), "r"(r2), "r"(r7) : "memory");
    return r0;
#else
    register long a0r __asm__("a0") = a0;
    register long a1r __asm__("a1") = a1;
    register long a2r __asm__("a2") = a2;
    register long a7r __asm__("a7") = nr;

    __asm__ volatile("ecall" : "+r"(a0r) : "r"(a1r), "r"(a2r), "r"(a7r) : "memory");
    return a0r;
#endif
}

static inline long os_write(int fd, const void *buf, unsigned long len)
{
    return os_syscall3(OS_SYS_WRITE, fd, (long)buf, (long)len);
}

static inline long os_read(int fd, void *buf, unsigned long len)
{
    return os_syscall3(OS_SYS_READ, fd, (long)buf, (long)len);
}

static inline int os_getpid(void)
{
    return (int)os_syscall3(OS_SYS_GETPID, 0, 0, 0);
}

static inline void os_yield(void)
{
    os_syscall3(OS_SYS_YIELD, 0, 0, 0);
}

static inline long os_fork(void)
{
    return os_syscall3(OS_SYS_FORK, 0, 0, 0);
}

static inline long os_sbrk(long increment)
{
    return os_syscall3(OS_SYS_SBRK, increment, 0, 0);
}

static inline long os_exec(const char *path)
{
    return os_syscall3(OS_SYS_EXEC, (long)path, 0, 0);
}

static inline long os_open(const char *path, int flags)
{
    return os_syscall3(OS_SYS_OPEN, (long)path, flags, 0);
}

static inline long os_close(int fd)
{
    return os_syscall3(OS_SYS_CLOSE, fd, 0, 0);
}

static inline long os_mkdir(const char *path)
{
    return os_syscall3(OS_SYS_MKDIR, (long)path, 0, 0);
}

static inline void os_ps(void)
{
    os_syscall3(OS_SYS_PS, 0, 0, 0);
}

static inline void os_exit(int code)
{
    os_syscall3(OS_SYS_EXIT, code, 0, 0);
    for (;;) {
    }
}

unsigned long os_strlen(const char *s);
int os_strcmp(const char *a, const char *b);
int os_strncmp(const char *a, const char *b, unsigned long n);
void os_puts(const char *s);
void os_putchar(char c);

#endif
