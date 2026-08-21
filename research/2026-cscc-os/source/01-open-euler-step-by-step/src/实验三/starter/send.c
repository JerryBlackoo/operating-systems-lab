/*
 * starter/send.c - 消息队列发送端（学生填空版）
 * TODO: 补全代码，实现消息队列发送
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/ipc.h>
#include <sys/msg.h>

#define KEY_PATH "/tmp"
#define KEY_ID 12345

/* TODO: 定义消息结构体 my_msg，包含 msg_type 和 msg_text */

int main() {
    /* TODO: 生成key并创建消息队列 */
    
    struct my_msg msg;
    msg.msg_type = 1;
    
    printf("消息发送端，输入消息 (输入 'end' 退出):\n");
    while (1) {
        printf("> ");
        fgets(msg.msg_text, 512, stdin);
        msg.msg_text[strcspn(msg.msg_text, "\n")] = 0;
        
        /* TODO: 使用 msgsnd() 发送消息 */
        printf("已发送: %s\n", msg.msg_text);
        
        if (strcmp(msg.msg_text, "end") == 0) break;
    }
    return 0;
}