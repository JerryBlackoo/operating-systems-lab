#ifndef MICRO_BOOT_FDT_INITRD_H
#define MICRO_BOOT_FDT_INITRD_H

#include "types.h"

int fdt_chosen_initrd(u64 dtb, u64 *start, u64 *end);

#endif