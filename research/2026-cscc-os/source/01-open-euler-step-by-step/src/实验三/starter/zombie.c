/*
 * starter/zombie.c - 僵尸进程（学生填空版）
 * TODO: 补全代码，创建子进程并观察僵尸进程
 */

#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/wait.h>

int main() {
    /* TODO: 调用 fork() 创建子进程 */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
    
    if (/* TODO: 判断fork返回值 */) {
        perror("fork失败");
        exit(1);
    } 
    else if (/* TODO: 子进程分支 */) {
        printf("子进程 PID: %d, 父进程 PID: %d\n", getpid(), getppid());
        printf("子进程结束\n");
        return 0;
    } 
    else {
        printf("父进程 PID: %d, 创建了子进程 PID: %d\n", getpid(), /* TODO: 填入子进程PID */);
        printf("父进程睡眠30秒，期间子进程会变成僵尸进程\n");
        printf("请在另一个终端执行: ps aux | grep Z\n");
        sleep(30);
        printf("父进程结束，僵尸进程被回收\n");
        return 0;
    }
}