#include "syscall/syscall.h"

#include "console.h"
#include "dev/device.h"
#include "fs/fs.h"
#include "lib/kprintf.h"
#include "lib/string.h"
#include "memory/memory.h"
#include "mm/trap_vm.h"
#include "mm/vm.h"
#include "proc/proc.h"
#include "proc/user.h"
#include "shared/abi/syscall.h"

#define COPY_CHUNK 128

/* 控制台读等待队列：UART 没有 RX 中断，由定时器 tick 轮询到数据后唤醒。
 * 读进程因此进入 SLEEPING，idle 可以在 wfi 中等待，而不是 100% 空转。 */
static struct proc *console_waiters[MAX_PROCS];
static int console_nwait;
/* 上一行由 '\r' 结束时置位：CRLF 终端紧随的 '\n' 属于同一行尾，
 * 下一次 read 直接丢弃，避免被当成一条空命令。 */
static int console_skip_lf;

static int console_waiter_add(struct proc *p)
{
    for (int i = 0; i < console_nwait; i++) {
        if (console_waiters[i] == p) {
            return 1;
        }
    }
    if (console_nwait >= MAX_PROCS) {
        return 0;
    }
    console_waiters[console_nwait++] = p;
    return 1;
}

static void console_waiter_remove(struct proc *p)
{
    for (int i = 0; i < console_nwait; i++) {
        if (console_waiters[i] != p) {
            continue;
        }
        for (int j = i + 1; j < console_nwait; j++) {
            console_waiters[j - 1] = console_waiters[j];
        }
        console_nwait--;
        return;
    }
}

void console_input_poll(void)
{
    if (console_nwait == 0 || console_rx_ready() == 0) {
        return;
    }
    for (int i = 0; i < console_nwait; i++) {
        struct proc *p = console_waiters[i];

        if (p != 0 && p->state == PROC_SLEEPING) {
            p->state = PROC_READY;
        }
    }
    console_nwait = 0;
}

/* 用户页可能已按需换出：翻译失败时先尝试补页，再重新翻译。 */
static uint64_t user_translate(uint64_t pagetable, uint64_t va)
{
    uint64_t pa = vm_translate(pagetable, va);

    if (pa == 0 && current != 0 && current->is_user != 0 &&
        pagetable == current->pagetable && vm_handle_fault(va, 0, 0) != 0) {
        pa = vm_translate(pagetable, va);
    }
    return pa;
}

static uint64_t user_copy_in(uint64_t pagetable, void *dst, uint64_t uva, uint64_t len)
{
    uint8_t *out = (uint8_t *)dst;
    uint64_t done = 0;

    while (done < len) {
        uint64_t va = uva + done;
        uint64_t pa;
        uint64_t page_left;
        uint64_t n = len - done;

        if (va < USER_BASE || va >= USER_STACK_TOP) {
            return done;
        }
        pa = user_translate(pagetable, va);
        page_left = PAGE_SIZE - (va & (PAGE_SIZE - 1));
        if (pa == 0) {
            return done;
        }
        if (n > page_left) {
            n = page_left;
        }
        memcpy(out + done, (void *)(uintptr_t)pa, n);
        done += n;
    }
    return done;
}

static uint64_t user_copy_out(uint64_t pagetable, uint64_t uva, const void *src, uint64_t len)
{
    const uint8_t *in = (const uint8_t *)src;
    uint64_t done = 0;

    while (done < len) {
        uint64_t va = uva + done;
        uint64_t pa;
        uint64_t page_left;
        uint64_t n = len - done;

        if (va < USER_BASE || va >= USER_STACK_TOP) {
            return done;
        }
        pa = user_translate(pagetable, va);
        page_left = PAGE_SIZE - (va & (PAGE_SIZE - 1));
        if (pa == 0) {
            return done;
        }
        if (n > page_left) {
            n = page_left;
        }
        memcpy((void *)(uintptr_t)pa, in + done, n);
        done += n;
    }
    return done;
}

static int copyinstr(uint64_t pagetable, char *dst, uint64_t uva, uint64_t max)
{
    for (uint64_t i = 0; i < max; i++) {
        uint64_t va = uva + i;
        uint64_t pa;
        char c;

        if (va < USER_BASE || va >= USER_STACK_TOP) {
            return -1;
        }
        pa = user_translate(pagetable, va);
        if (pa == 0) {
            return -1;
        }
        c = *(const char *)(uintptr_t)pa;
        dst[i] = c;
        if (c == '\0') {
            return 0;
        }
    }
    /* 缓冲区已填满仍未遇到 '\0'：按错误处理，避免把截断后的路径当成有效路径。 */
    dst[max - 1] = '\0';
    return -1;
}

static int64_t sys_fork(void)
{
    struct proc *child = proc_fork();

    if (child == 0) {
        return OS_ENOMEM;
    }
    return child->pid;
}

static int64_t sys_exec(uint64_t uva)
{
    char name[32];

    if (copyinstr(current->pagetable, name, uva, sizeof(name)) != 0) {
        return OS_EINVAL;
    }
    return user_exec_current(name);
}

static int64_t sys_sbrk(int64_t delta)
{
    uint64_t old = current->heap_top;
    uint64_t limit = USER_HEAP_LIMIT;
    uint64_t new_top = old;

    if (old < USER_HEAP_BASE || old > limit) {
        return OS_ENOMEM;
    }
    if (delta > 0) {
        if ((uint64_t)delta > limit - old) {
            return OS_ENOMEM;
        }
        new_top = old + (uint64_t)delta;
    } else if (delta < 0) {
        /* 先转成正的跨度，避免 INT64_MIN 取反溢出。 */
        uint64_t drop = (uint64_t)(-(delta + 1)) + 1;

        if (drop > old - USER_HEAP_BASE) {
            return OS_EINVAL;
        }
        new_top = old - drop;
        /* 收缩的堆页要立即回收：它们已在 heap_top 之外，换出和退出扫描都不会再处理。 */
        vm_release_range(current->pagetable, new_top, old);
    }
    current->heap_top = new_top;
    return (int64_t)old;
}

static struct file *fd_file(uint64_t fd)
{
    if (fd < 3 || fd >= 8 || current->fd[fd] <= 0) {
        return 0;
    }
    int index = current->fd[fd] - 1;
    if (index < 0 || index >= FS_MAX_FILES) {
        return 0;
    }
    /* fs.c 的 files[] 是静态的；通过 fs_file_alloc 的返回值无法直接索引，
     * 这里用一个访问器替代。 */
    return fs_file_at(index);
}

static int64_t sys_open(uint64_t uva_path, uint64_t flags)
{
    char path[64];
    struct file *f;
    int fd;
    int ino = -1;
    int dev = 0;
    int index;

    if (copyinstr(current->pagetable, path, uva_path, sizeof(path)) != 0) {
        return OS_EINVAL;
    }
    /* flags 低两位是访问模式；3 未定义，拒绝而不是带着无效模式打开。 */
    if ((flags & 3) == 3) {
        return OS_EINVAL;
    }
    /* O_TRUNC 必须带写权限；在创建文件之前拒绝，避免失败却留下新文件。 */
    if ((flags & OS_O_TRUNC) != 0 && (flags & 3) == OS_O_RDONLY) {
        return OS_EPERM;
    }

    /* 先预留 fd 与文件表槽位，再做路径创建/截断；否则 ENOMEM/EBADF
     * 会让调用失败但文件已经被创建或清空。 */
    for (fd = 3; fd < 8; fd++) {
        if (current->fd[fd] <= 0) {
            break;
        }
    }
    if (fd >= 8) {
        return OS_EBADF;
    }
    f = fs_file_alloc();
    if (f == 0) {
        return OS_ENOMEM;
    }

    if (strncmp(path, "/dev/", 5) == 0) {
        /* 设备号即 devices[] 下标；0 是 console，当前未实现为可 open 设备。 */
        dev = dev_lookup(path + 5);
        if (dev <= 0) {
            fs_file_free(f);
            return OS_ENOENT;
        }
    } else {
        ino = fs_lookup(path);
        if (ino < 0 && (flags & OS_O_CREATE) != 0) {
            ino = fs_create(path, 1);
        }
        if (ino < 0) {
            fs_file_free(f);
            return OS_ENOENT;
        }
    }

    if (dev == 0 && (flags & OS_O_TRUNC) != 0) {
        if (fs_is_dir(ino) != 0) {
            fs_file_free(f);
            return OS_EINVAL;
        }
        if (fs_truncate(ino) != 0) {
            fs_file_free(f);
            return OS_EIO;
        }
    }

    f->ino = ino;
    f->dev = dev;
    f->off = 0;
    f->flags = (int)flags;
    index = fs_file_index(f);
    current->fd[fd] = index + 1;
    return fd;
}

static int64_t sys_close(uint64_t fd)
{
    struct file *f = fd_file(fd);

    if (f == 0) {
        return OS_EBADF;
    }
    fs_file_free(f);
    current->fd[fd] = -1;
    return 0;
}

static int64_t sys_write(uint64_t fd, uint64_t uva, uint64_t len)
{
    char buffer[COPY_CHUNK];
    uint64_t done = 0;

    if (fd >= 3) {
        struct file *f = fd_file(fd);
        if (f == 0) {
            return OS_EBADF;
        }
        /* 按 open 模式拒绝写只读 fd（stdio 的 0/1/2 不经过这里）。 */
        if ((f->flags & 3) != OS_O_WRONLY && (f->flags & 3) != OS_O_RDWR) {
            return OS_EBADF;
        }
        if (f->dev != 0) {
            /* 设备文件统一走 devsw；先拷入内核缓冲，再交给驱动。 */
            if (f->dev < 0 || f->dev >= device_count ||
                devices[f->dev].write == 0) {
                return OS_EIO;
            }
            uint64_t n = len;
            int wrote;

            if (n == 0) {
                return 0;
            }
            if (n > sizeof(buffer)) {
                n = sizeof(buffer);
            }
            if (user_copy_in(current->pagetable, buffer, uva, n) != n) {
                return OS_EIO;
            }
            wrote = devices[f->dev].write(buffer, (uint32_t)n);
            if (wrote <= 0) {
                return OS_EIO;
            }
            return wrote;
        }
        /* 目录的数据块是文件系统元数据；按普通文件写入会伪造目录项。 */
        if (fs_is_dir(f->ino) != 0) {
            return OS_EINVAL;
        }
        while (done < len) {
            uint64_t n = len - done;
            if (n > sizeof(buffer)) {
                n = sizeof(buffer);
            }
            if (user_copy_in(current->pagetable, buffer, uva + done, n) != n) {
                /* 已写入的部分要如实返回，调用方才能续写剩余数据。 */
                return done > 0 ? (int64_t)done : OS_EIO;
            }
            int wrote = fs_write(f->ino, f->off, buffer, (uint32_t)n);
            if (wrote <= 0) {
                return done > 0 ? (int64_t)done : OS_EIO;
            }
            f->off += (uint32_t)wrote;
            done += (uint64_t)wrote;
            if (wrote != (int)n) {
                /* 短写：返回已写入字节数；报错会让调用方在已前进的偏移上重试。 */
                return (int64_t)done;
            }
        }
        return (int64_t)len;
    }

    if (fd != 1 && fd != 2) {
        return OS_EBADF;
    }
    while (done < len) {
        uint64_t n = len - done;
        if (n > sizeof(buffer)) {
            n = sizeof(buffer);
        }
        if (user_copy_in(current->pagetable, buffer, uva + done, n) != n) {
            return done > 0 ? (int64_t)done : OS_EIO;
        }
        for (uint64_t i = 0; i < n; i++) {
            if (buffer[i] == '\n') {
                console_putchar('\r');
            }
            console_putchar(buffer[i]);
        }
        done += n;
    }
    return (int64_t)len;
}

static int64_t sys_read(uint64_t fd, uint64_t uva, uint64_t len)
{
    char buffer[COPY_CHUNK];
    uint64_t n = 0;

    if (fd >= 3) {
        struct file *f = fd_file(fd);
        if (f == 0) {
            return OS_EBADF;
        }
        /* 按 open 模式拒绝读只写 fd。 */
        if ((f->flags & 3) != OS_O_RDONLY && (f->flags & 3) != OS_O_RDWR) {
            return OS_EBADF;
        }
        if (f->dev != 0) {
            uint64_t n = len;
            uint64_t copied;
            int got;

            if (f->dev < 0 || f->dev >= device_count ||
                devices[f->dev].read == 0) {
                return OS_EIO;
            }
            if (n == 0) {
                return 0;
            }
            if (n > sizeof(buffer)) {
                n = sizeof(buffer);
            }
            got = devices[f->dev].read(buffer, (uint32_t)n);
            if (got < 0) {
                return OS_EIO;
            }
            if (got == 0) {
                return 0;
            }
            copied = user_copy_out(current->pagetable, uva, buffer, (uint64_t)got);
            if (copied != (uint64_t)got) {
                return copied > 0 ? (int64_t)copied : OS_EIO;
            }
            return got;
        }
        /* 目录数据是文件系统元数据；按普通文件读取只会把 dirent 原始字节
         * 泄露给用户，和 write 路径一样直接拒绝。 */
        if (fs_is_dir(f->ino) != 0) {
            return OS_EINVAL;
        }
        if (len > sizeof(buffer)) {
            len = sizeof(buffer);
        }
        int got = fs_read(f->ino, f->off, buffer, (uint32_t)len);
        if (got <= 0) {
            return 0;
        }
        /* 先确认整段数据已送达用户空间，再推进偏移；否则拷贝失败会
         * 丢掉这一段文件内容。 */
        if (user_copy_out(current->pagetable, uva, buffer, (uint64_t)got) != (uint64_t)got) {
            return OS_EIO;
        }
        f->off += (uint32_t)got;
        return got;
    }

    if (fd != 0) {
        return OS_EBADF;
    }
    /* 一次 read 必须吃掉整行：缓冲区满后继续消费到换行，只做截断，
     * 否则剩余字符会被下一次 read 当成一条新命令执行。 */
    uint64_t cap = len < sizeof(buffer) ? len : sizeof(buffer);

    while (cap > 0) {
        int c;

        /* 控制台按行缓冲：无字符时睡眠，由定时器 tick 轮询 UART 后
         * 唤醒；避免 yield 后立即被选回造成空转。 */
        while (console_rx_ready() == 0) {
            if (console_waiter_add(current) == 0) {
                /* 等待队列异常满：退回让出 CPU，避免永久睡死。 */
                yield();
                continue;
            }
            current->state = PROC_SLEEPING;
            sched();
            console_waiter_remove(current);
        }
        c = console_try_getchar();
        if (c < 0) {
            /* 与 tick 唤醒竞争：字符可能已被其他读者取走，重新等待。 */
            continue;
        }
        if (c == '\r') {
            console_skip_lf = 1;
            c = '\n';
        } else if (c == '\n') {
            if (console_skip_lf != 0) {
                /* 丢弃上一行 CRLF 的 LF，继续等待下一条命令行。 */
                console_skip_lf = 0;
                continue;
            }
        } else if (console_skip_lf != 0) {
            /* 不是 LF：说明是 CR-only 终端，恢复普通字符处理。 */
            console_skip_lf = 0;
        }
        if (c == '\n') {
            console_putchar('\r');
            console_putchar('\n');
            if (n < cap) {
                buffer[n++] = '\n';
            }
            break;
        }
        if (c == 8 || c == 127) {
            if (n > 0) {
                n--;
                console_write("\b \b");
            }
            continue;
        }
        if (n < cap) {
            console_putchar((char)c);
            buffer[n++] = (char)c;
        }
    }
    if (n == 0) {
        return 0;
    }
    if (user_copy_out(current->pagetable, uva, buffer, n) != n) {
        return OS_EIO;
    }
    return (int64_t)n;
}

#ifdef PLATFORM_ZYNQ
/* ARM SVC 入口用 32 位寄存器传参；转成公共 64 位 ABI。 */
int64_t arm_syscall_dispatch(uint32_t nr, uint32_t a0, uint32_t a1, uint32_t a2)
{
    if (nr == OS_SYS_SBRK) {
        /* sbrk 的增量是有符号 32 位；零扩展会把 -4096 变成巨大的正数。 */
        return syscall_dispatch(nr, (uint64_t)(int64_t)(int32_t)a0, a1, a2);
    }
    return syscall_dispatch(nr, a0, a1, a2);
}
#endif

void proc_exit_cleanup(void)
{
    uint64_t old_pt = current->pagetable;
    uint64_t old_heap = current->heap_top;

    console_waiter_remove(current);

    /* 归还该进程占用的文件表槽位，避免退出时泄漏。 */
    for (int fd = 3; fd < 8; fd++) {
        if (current->fd[fd] > 0) {
            struct file *f = fd_file((uint64_t)fd);
            if (f != 0) {
                fs_file_free(f);
            }
            current->fd[fd] = -1;
        }
    }
    vm_release_pid(current->pid);
    if (old_pt != vm_kernel_pagetable()) {
        vm_switch(vm_kernel_pagetable());
        current->pagetable = vm_kernel_pagetable();
        uvm_destroy(old_pt, old_heap);
    }
}

int64_t syscall_dispatch(uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2)
{
    switch (nr) {
    case OS_SYS_NOP:
        return 0;
    case OS_SYS_WRITE:
        return sys_write(a0, a1, a2);
    case OS_SYS_READ:
        return sys_read(a0, a1, a2);
    case OS_SYS_EXIT: {
        current->exit_code = (int)a0;
        vm_stats_print();
        proc_exit_cleanup();
        current->state = PROC_ZOMBIE;
        yield();
        return 0;
    }
    case OS_SYS_FORK:
        return sys_fork();
    case OS_SYS_EXEC:
        return sys_exec(a0);
    case OS_SYS_SBRK:
        return sys_sbrk((int64_t)a0);
    case OS_SYS_OPEN:
        return sys_open(a0, a1);
    case OS_SYS_CLOSE:
        return sys_close(a0);
    case OS_SYS_MKDIR: {
        char path[64];
        if (copyinstr(current->pagetable, path, a0, sizeof(path)) != 0) {
            return OS_EINVAL;
        }
        if (fs_lookup(path) >= 0) {
            return OS_EEXIST;
        }
        return fs_mkdir(path) >= 0 ? 0 : OS_EIO;
    }
    case OS_SYS_GETPID:
        return current->pid;
    case OS_SYS_YIELD:
        yield();
        return 0;
    case OS_SYS_PS:
        proc_dump();
        return 0;
    default:
        kprintf("[syscall] bad nr=%d\n", (int)nr);
        return OS_ENOSYS;
    }
}
