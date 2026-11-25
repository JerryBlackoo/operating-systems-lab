#ifndef MICRO_LOADER_LOADER_H
#define MICRO_LOADER_LOADER_H

#include <stddef.h>
#include <stdint.h>

#include "bootinfo.h"
#include "memory.h"
#include "sched.h"

#define USER_STACK_VA 0x40100000UL

/* loader_spawn_user 的失败分类；PM 据此回复真实错误码。 */
enum loader_status {
    LOADER_OK = 0,
    LOADER_EINVAL = -1,
    LOADER_ENOENT = -2,
    LOADER_ENOMEM = -3,
    LOADER_EFULL = -4,
    LOADER_EMAP = -5,
};

struct loader_image {
    struct as *as;
    vaddr_t entry;
    vaddr_t stack_top;
};

struct loader_boot_args {
    uint8_t cap_sid;
    uint64_t a0;
    uint64_t a1;
    uint64_t a2;
    uint64_t a3;
    uint64_t a4;
    uint64_t a5;
};

/* 将 initrd 拷到帧池外镜像，避免后续 frame_alloc 覆写 QEMU initrd 物理页。 */
int loader_pin_initrd(const struct bootinfo *bi);

int loader_find_initrd(const void *base, size_t size, const char *name,
                       const void **image, size_t *image_size);

int loader_load_elf(struct as *as, const void *elf, size_t elf_size,
                    struct loader_image *out);

int loader_spawn_user(const struct bootinfo *bi, pfn_t kernel_root,
                      const char *initrd_name, const struct loader_boot_args *boot,
                      struct thread **thread_out);

/* PM spawn 后续步骤失败时回收 loader_spawn_user 产物（launch 槽 + 线程/AS）。 */
int loader_abort_spawn(struct thread *t);

/*
 * 终止一个可能已经跑起来的子进程（PM discard 用）。
 *
 * 与 loader_abort_spawn 的区别：后者只适用于尚未运行用户入口的回滚，
 * 遇到 RUNNING 线程直接失败。本函数对 RUNNING 线程登记延迟退出，对阻塞在
 * 异步 syscall 上的线程先撤销在途 reply 再摘除。返回 0 表示已摘除或已登记。
 */
int loader_terminate_spawn(struct thread *t);

#endif