#include <stdio.h>
#include "../src/xiji_runner.h"

typedef int (*test_fn)(void);

static const struct {
    const char *name;
    test_fn fn;
} tests[] = {
    { "exp01", xiji_exp01_run },
    { "exp02", xiji_exp02_run },
    { "exp03", xiji_exp03_run },
    { "exp04", xiji_exp04_run },
    { "exp05", xiji_exp05_run },
    { "exp06", xiji_exp06_run },
    { "exp07", xiji_exp07_run },
};

int main(void)
{
    int failed = 0;
    for (unsigned i = 0; i < sizeof tests / sizeof tests[0]; i++) {
        if (tests[i].fn() != 0)
            failed++;
    }
    if (failed)
        return 1;
    fputs("ALL PASS (host)\n", stdout);
    return 0;
}