#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/ipc.h>
#include <sys/shm.h>

#define KEY_PATH "/tmp"
#define KEY_ID 67890

// TODO: 定义共享内存数据结构
struct shared_data {
    int written;
    char text[2048];
};

int main() {
    // TODO: 生成key并获取共享内存
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
    
    // TODO: 附加共享内存
    struct shared_data *shared = shmat(shmid, NULL, 0);
    if (shared == (void*)-1) {
        perror("shmat failed");
        exit(1);
    }
    
    printf("共享内存读端，等待数据...\n");
    int running = 1;
    
    while (running) {
        // TODO: 检查是否有新数据
        if (shared->written == 1) {
            printf("读到: %s\n", shared->text);
            
            if (strcmp(shared->text, "end") == 0) {
                running = 0;
            }
            
            // TODO: 重置标志
            shared->written = 0;
        } else {
            usleep(100000);
        }
    }
    
    // TODO: 分离并删除共享内存
    shmdt(shared);
    shmctl(shmid, IPC_RMID, NULL);
    printf("共享内存已删除\n");
    return 0;
}