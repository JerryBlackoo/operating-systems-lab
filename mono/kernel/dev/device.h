#ifndef KERNEL_DEV_DEVICE_H
#define KERNEL_DEV_DEVICE_H

#include <stdint.h>

struct devsw {
    const char *name;
    int (*read)(void *buf, uint32_t len);
    int (*write)(const void *buf, uint32_t len);
};

extern struct devsw devices[];
extern const int device_count;

int dev_lookup(const char *name);
void dev_selftest(void);

#endif
