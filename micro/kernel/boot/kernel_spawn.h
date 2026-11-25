#ifndef MICRO_KERNEL_BOOT_KERNEL_SPAWN_H
#define MICRO_KERNEL_BOOT_KERNEL_SPAWN_H

#include <stdint.h>

#include "bootinfo.h"
#include "capability.h"
#include "memory.h"
#include "sched.h"

enum kernel_spawn_image {
    SPAWN_FS = 1,
    SPAWN_CONSOLE = 2,
    SPAWN_SHELL = 3,
    SPAWN_VM = 4,
    SPAWN_HEAPTEST = 5,
    SPAWN_HEAPTEST2 = 6,
};

void kernel_spawn_init(const struct bootinfo *bi, pfn_t kernel_root);

/* reserved 对应 ABI a2；cap 组合由内核固定，用户态不能借它扩权。 */
int64_t kernel_pm_spawn(uint8_t caller_sid, uint64_t parent, uint32_t image,
                        cap_rights_t reserved, uint64_t *pid_out,
                        uint64_t *tid_out, cap_handle_t *pm_as_cap_out);

int64_t kernel_cap_mint_to(uint8_t sm_sid, cap_handle_t src, cap_sid_t dst_sid,
                           cap_rights_t rights, cap_badge_t badge,
                           cap_handle_t *out);

int64_t kernel_pm_discard(uint8_t caller_sid, uint32_t thread_id,
                          uint32_t image);

#endif