#include "userland/lib.h"

int main(void)
{
    os_write(1, "hello from user\n", 16);
    return 0;
}
