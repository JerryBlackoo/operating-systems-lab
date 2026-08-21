/*
 * starter/write.c - 共享内存写端（学生填空版）
 * TODO: 补全代码，实现共享内存写入
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

/* TODO: 定义共享内存数据结构 shared_data，包含 written 和 text */

int main() {
    /* TODO: 生成key并创建共享内存 */
    
    /* TODO: 附加共享内存 */
    
    /* TODO: 初始化 written 标志为 0 */
    
    printf("共享内存写端，输入消息 (输入 'end' 退出):\n");
    char buffer[2048];
    
    while (1) {
        /* TODO: 等待读端读取完成 (written == 1 时等待) */
        
        printf("> ");
        fflush(stdout);
        fgets(buffer, 2048, stdin);
        buffer[strcspn(buffer, "\n")] = 0;
        
        /* TODO: 将数据写入共享内存，设置 written = 1 */
        printf("已写入: %s\n", buffer);
        
        if (strcmp(buffer, "end") == 0) break;
    }
    
    /* TODO: 分离共享内存 */
    return 0;
}