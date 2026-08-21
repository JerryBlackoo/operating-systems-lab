/*
 * solution/read_answer.c - 共享内存读端（完整答案）
 * 功能：从共享内存读取数据
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
    
    int shmid = shmget(key, sizeof(struct shared_data), 0666);
    if (shmid == -1) {
        perror("shmget failed");
        exit(1);
    }
    
    struct shared_data *shared = shmat(shmid, NULL, 0);
    if (shared == (void*)-1) {
        perror("shmat failed");
        exit(1);
    }
    
    printf("共享内存读端，等待数据...\n");
    int running = 1;
    
    while (running) {
        if (shared->written == 1) {
            printf("读到: %s\n", shared->text);
            
            if (strcmp(shared->text, "end") == 0) {
                running = 0;
            }
            
            shared->written = 0;
        } else {
            usleep(100000);
        }
    }
    
    shmdt(shared);
    shmctl(shmid, IPC_RMID, NULL);
    printf("共享内存已删除\n");
    return 0;
}