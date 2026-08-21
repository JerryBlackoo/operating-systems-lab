#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <unistd.h>

pthread_t ntid;

void *thr_fn(void *arg) {
    // TODO: 打印新线程ID
    printf("新线程ID: %lu\n", pthread_self());
    return NULL;
}

int main() {
    // TODO: 获取并打印主线程ID
    pthread_t main_thread = pthread_self();
    printf("主线程ID: %lu\n", main_thread);
    
    // TODO: 创建子线程
    int err = pthread_create(&ntid, NULL, thr_fn, NULL);
    if (err != 0) {
        printf("创建线程失败\n");
        exit(1);
    }
    
    printf("创建的子线程ID: %lu\n", ntid);
    
    // TODO: 等待子线程结束
    pthread_join(ntid, NULL);
    return 0;
}