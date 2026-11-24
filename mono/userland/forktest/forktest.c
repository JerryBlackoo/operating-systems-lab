#include "userland/lib.h"

int main(void)
{
    long bad = os_syscall3(999, 0, 0, 0);
    long pid;

    if (bad < 0) {
        os_puts("bad syscall returned negative\n");
    }

    pid = os_fork();

    if (pid == 0) {
        os_puts("child: fork returned 0\n");
    } else if (pid > 0) {
        os_puts("parent: fork returned child pid\n");
    } else {
        os_puts("fork failed\n");
    }
    return 0;
}
