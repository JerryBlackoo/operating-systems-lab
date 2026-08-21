/*
 * solution/send_answer.c - 消息队列发送端（完整答案）
 * 功能：创建消息队列并发送消息
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/ipc.h>
#include <sys/msg.h>

#define KEY_PATH "/tmp"
#define KEY_ID 12345

struct my_msg {
    long msg_type;
    char msg_text[512];
};

int main() {
    key_t key = ftok(KEY_PATH, KEY_ID);
    int msgid = msgget(key, 0666 | IPC_CREAT);
    
    struct my_msg msg;
    msg.msg_type = 1;
    
    printf("消息发送端，输入消息 (输入 'end' 退出):\n");
    while (1) {
        printf("> ");
        fgets(msg.msg_text, 512, stdin);
        msg.msg_text[strcspn(msg.msg_text, "\n")] = 0;
        
        msgsnd(msgid, &msg, strlen(msg.msg_text) + 1, 0);
        printf("已发送: %s\n", msg.msg_text);
        
        if (strcmp(msg.msg_text, "end") == 0) break;
    }
    return 0;
}