/*
 * solution/threadid_answer.c - 线程通信（完整答案）
 * 功能：创建线程并打印主线程/子线程ID
 */

#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <unistd.h>

pthread_t ntid;

void *thr_fn(void *arg) {
    printf("新线程ID: %lu\n", pthread_self());
    return NULL;
}

int main() {
    pthread_t main_thread = pthread_self();
    printf("主线程ID: %lu\n", main_thread);
    
    int err = pthread_create(&ntid, NULL, thr_fn, NULL);
    if (err != 0) {
        printf("创建线程失败\n");
        exit(1);
    }
    
    printf("创建的子线程ID: %lu\n", ntid);
    pthread_join(ntid, NULL);
    return 0;
}