/*
 * starter/getpid.c - 获取进程号（学生填空版）
 * TODO: 补全代码，获取当前进程PID并打印
 */

#include <unistd.h>
#include <stdio.h>

int main() {
    /* TODO: 调用 getpid() 获取当前进程PID并打印 */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
    
    printf("请另开终端执行: ps -ef | grep %d\n", /* 填入PID */);
    printf("按 Ctrl+C 终止进程\n");
    sleep(30);
    return 0;
}