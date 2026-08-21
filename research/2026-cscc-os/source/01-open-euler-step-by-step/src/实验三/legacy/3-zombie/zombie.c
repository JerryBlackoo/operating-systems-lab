#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/wait.h>

int main() {
    // TODO: 创建子进程
    pid_t child_pid = fork();
    
    if (child_pid < 0) {
        perror("fork失败");
        exit(1);
    } 
    else if (child_pid == 0) {
        // TODO: 子进程执行，打印PID后退出
        printf("子进程 PID: %d, 父进程 PID: %d\n", getpid(), getppid());
        printf("子进程结束\n");
        return 0;
    } 
    else {
        // TODO: 父进程打印信息，睡眠30秒，观察僵尸进程
        printf("父进程 PID: %d, 创建了子进程 PID: %d\n", getpid(), child_pid);
        printf("父进程睡眠30秒，期间子进程会变成僵尸进程\n");
        printf("请在另一个终端执行: ps aux | grep Z\n");
        sleep(30);
        // TODO: 父进程结束，子进程被init回收
        printf("父进程结束，僵尸进程被回收\n");
        return 0;
    }
}