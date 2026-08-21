#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/ipc.h>
#include <sys/msg.h>

#define KEY_PATH "/tmp"
#define KEY_ID 12345

// TODO: 定义消息结构体
struct my_msg {
    long msg_type;
    char msg_text[512];
};

int main() {
    // TODO: 生成key并获取消息队列
    key_t key = ftok(KEY_PATH, KEY_ID);
    int msgid = msgget(key, 0666 | IPC_CREAT);
    
    struct my_msg msg;
    
    printf("消息接收端，等待消息...\n");
    while (1) {
        // TODO: 接收消息
        msgrcv(msgid, &msg, 512, 0, 0);
        printf("收到: %s\n", msg.msg_text);
        
        if (strcmp(msg.msg_text, "end") == 0) break;
    }
    
    // TODO: 删除消息队列
    msgctl(msgid, IPC_RMID, NULL);
    printf("消息队列已删除\n");
    return 0;
}