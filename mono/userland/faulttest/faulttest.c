#include "userland/lib.h"

#define PAGES 96

int main(void)
{
    char *base = (char *)os_sbrk(PAGES * 4096);
    int bad = 0;

    if ((long)base < 0) {
        os_puts("sbrk failed\n");
        return 1;
    }

    for (int i = 0; i < PAGES; i++) {
        base[i * 4096] = (char)(i + 1);
    }
    for (int i = 0; i < PAGES; i++) {
        if (base[i * 4096] != (char)(i + 1)) {
            bad++;
        }
    }

    if (bad != 0) {
        os_puts("faulttest: data mismatch\n");
    } else {
        os_puts("faulttest: demand paging and swap ok\n");
    }
    return 0;
}
