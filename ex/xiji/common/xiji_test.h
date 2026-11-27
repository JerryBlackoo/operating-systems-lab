#ifndef XIJI_TEST_H
#define XIJI_TEST_H

#include "xiji_io.h"

static inline int xiji_fail(const char *name, const char *why)
{
    xiji_puts("FAIL ");
    xiji_puts(name);
    xiji_puts(": ");
    xiji_puts(why);
    xiji_putc('\n');
    return 1;
}

#define XIJI_CHECK(name, cond) \
    do { \
        if (!(cond)) \
            return xiji_fail(name, #cond); \
    } while (0)

#endif