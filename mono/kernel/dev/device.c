#include "dev/device.h"

#include "lib/kprintf.h"
#include "lib/string.h"

static int null_read(void *buf, uint32_t len)
{
    (void)buf;
    (void)len;
    return 0;
}

static int null_write(const void *buf, uint32_t len)
{
    (void)buf;
    return (int)len;
}

static int zero_read(void *buf, uint32_t len)
{
    memset(buf, 0, len);
    return (int)len;
}

static int zero_write(const void *buf, uint32_t len)
{
    (void)buf;
    return (int)len;
}

struct devsw devices[] = {
    {"console", 0, 0},
    {"null", null_read, null_write},
    {"zero", zero_read, zero_write},
};

const int device_count = (int)(sizeof(devices) / sizeof(devices[0]));

int dev_lookup(const char *name)
{
    for (int i = 0; i < device_count; i++) {
        if (strcmp(devices[i].name, name) == 0) {
            return i;
        }
    }
    return -1;
}

void dev_selftest(void)
{
    char buf[8];

    (void)null_write("discard", 7);
    (void)zero_read(buf, sizeof(buf));
    kprintf("[dev] null/zero ok (devices=%d)\n", device_count);
}
