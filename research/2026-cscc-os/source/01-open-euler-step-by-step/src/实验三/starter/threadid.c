/*
 * starter/threadid.c - 线程通信（学生填空版）
 * TODO: 补全代码，创建线程并打印线程ID
 */

#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <unistd.h>

pthread_t ntid;

void *thr_fn(void *arg) {
    /* TODO: 打印新线程ID */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
    return NULL;
}

int main() {
    /* TODO: 打印主线程ID */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
    
    /* TODO: 创建子线程 */
    int err = pthread_create(&ntid, NULL, thr_fn, NULL);
    if (err != 0) {
        printf("创建线程失败\n");
        exit(1);
    }
    
    /* TODO: 等待子线程结束 */
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
    return 0;
}