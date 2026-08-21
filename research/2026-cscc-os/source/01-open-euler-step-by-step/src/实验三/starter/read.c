/*
 * starter/read.c - 共享内存读端（学生填空版）
 * TODO: 补全代码，实现共享内存读取
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/ipc.h>
#include <sys/shm.h>

#define KEY_PATH "/tmp"
#define KEY_ID 67890

/* TODO: 定义共享内存数据结构 shared_data */

int main() {
    /* TODO: 生成key并获取共享内存 */
    
    /* TODO: 附加共享内存 */
    
    printf("共享内存读端，等待数据...\n");
    int running = 1;
    
    while (running) {
        /* TODO: 检查是否有新数据 (written == 1) */
        if (/* TODO: 条件 */) {
            printf("读到: %s\n", shared->text);
            
            if (strcmp(shared->text, "end") == 0) {
                running = 0;
            }
            
            /* TODO: 重置 written 标志为 0 */
        } else {
            usleep(100000);
        }
    }
    
    /* TODO: 分离并删除共享内存 */
    printf("共享内存已删除\n");
    return 0;
}