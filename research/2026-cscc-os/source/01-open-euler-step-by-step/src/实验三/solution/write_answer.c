/*
 * solution/write_answer.c - 共享内存写端（完整答案）
 * 功能：向共享内存写入数据
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

struct shared_data {
    int written;
    char text[2048];
};

int main() {
    key_t key = ftok(KEY_PATH, KEY_ID);
    if (key == -1) {
        perror("ftok failed");
        exit(1);
    }
    
    int shmid = shmget(key, sizeof(struct shared_data), 0666 | IPC_CREAT);
    if (shmid == -1) {
        perror("shmget failed");
        exit(1);
    }
    
    struct shared_data *shared = shmat(shmid, NULL, 0);
    if (shared == (void*)-1) {
        perror("shmat failed");
        exit(1);
    }
    
    shared->written = 0;
    
    printf("共享内存写端，输入消息 (输入 'end' 退出):\n");
    char buffer[2048];
    
    while (1) {
        while (shared->written == 1) {
            printf("等待读端读取...\n");
            sleep(1);
        }
        
        printf("> ");
        fflush(stdout);
        fgets(buffer, 2048, stdin);
        buffer[strcspn(buffer, "\n")] = 0;
        
        strcpy(shared->text, buffer);
        shared->written = 1;
        
        printf("已写入: %s\n", buffer);
        
        if (strcmp(buffer, "end") == 0) break;
    }
    
    shmdt(shared);
    return 0;
}