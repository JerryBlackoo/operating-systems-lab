#include "userland/lib.h"

int main(void)
{
    os_puts("\n[init] root filesystem ready\n");
    os_puts("[demo] CPU utilization: single-program ~40%, multiprogram ~80%\n");
    os_puts("[init] exec /sh\n");

    if (os_exec("sh") < 0) {
        os_puts("[init] cannot exec sh\n");
    }
    return 0;
}
