/*
 * solution/getpid_answer.c - 获取进程号（完整答案）
 * 功能：调用 getpid() 获取当前进程PID并打印
 */

#include <unistd.h>
#include <stdio.h>

int main() {
    int pid = getpid();
    printf("进程号: %d\n", pid);
    printf("请另开终端执行: ps -ef | grep %d\n", pid);
    printf("按 Ctrl+C 终止进程\n");
    sleep(30);
    return 0;
}