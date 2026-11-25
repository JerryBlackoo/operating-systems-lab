/*
 * kernel_init_m6.c — **实验**：PM-only 启动（勿加入 Makefile C_SRCS）。
 *
 * 复用 kernel_init.c，但拦截内核对 vm/heaptest 的 loader_spawn_user。
 * 默认 capability 镜像必须用 kernel_init.c（混合启动）。
 */
#include "kernel_init.h"

#include "bootinfo.h"
#include "loader/loader.h"

static int name_is(const char *a, const char *b)
{
    while (*a != '\0' && *b != '\0') {
        if (*a++ != *b++) {
            return 0;
        }
    }
    return *a == '\0' && *b == '\0';
}

int loader_spawn_user_m6(const struct bootinfo *bi, pfn_t kernel_root,
                         const char *initrd_name,
                         const struct loader_boot_args *boot,
                         struct thread **thread_out)
{
    if (name_is(initrd_name, "vm") || name_is(initrd_name, "heaptest") ||
        name_is(initrd_name, "heaptest2")) {
        /* 由 init -> PM 的 ABI_PM_SPAWN 负责；内核路径只报告成功。 */
        return 0;
    }
    return loader_spawn_user(bi, kernel_root, initrd_name, boot, thread_out);
}

#define loader_spawn_user loader_spawn_user_m6
#include "kernel_init.c"
#undef loader_spawn_user