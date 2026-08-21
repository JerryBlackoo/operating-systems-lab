/*
 * starter/receive.c - 消息队列接收端（学生填空版）
 * TODO: 补全代码，实现消息队列接收
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/ipc.h>
#include <sys/msg.h>

#define KEY_PATH "/tmp"
#define KEY_ID 12345

/* TODO: 定义消息结构体 my_msg */

int main() {
    /* TODO: 生成key并获取消息队列 */
    
    struct my_msg msg;
    
    printf("消息接收端，等待消息...\n");
    while (1) {
        /* TODO: 使用 msgrcv() 接收消息 */
        printf("收到: %s\n", msg.msg_text);
        
        if (strcmp(msg.msg_text, "end") == 0) break;
    }
    
    /* TODO: 使用 msgctl() 删除消息队列 */
    printf("消息队列已删除\n");
    return 0;
}