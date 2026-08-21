#include <unistd.h>
#include <stdio.h>

int main() {
    // TODO: 获取当前进程PID并打印
    printf("进程号: %d\n", getpid());
    printf("请另开终端执行: ps -ef | grep %d\n", getpid());
    printf("按 Ctrl+C 终止进程\n");
    sleep(30);
    return 0;
}