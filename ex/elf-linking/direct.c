#include <stdio.h>
#include <unistd.h>
int add(int, int);
int main(void) {
    write(1, "[main] started\n", 15);
    int value = add(2, 3);
    printf("[main] add(2,3) = %d\n", value);
    return 0;
}
