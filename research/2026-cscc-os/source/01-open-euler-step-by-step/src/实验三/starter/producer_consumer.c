/*
 * starter/producer_consumer.c - 生产者消费者（学生填空版）
 * TODO: 补全代码，使用信号量解决生产者-消费者问题
 */

#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <semaphore.h>
#include <unistd.h>

#define BUFFER_SIZE 5
#define NUM_ITEMS 10

int buffer[BUFFER_SIZE];
int in = 0, out = 0;
/* TODO: 声明信号量 empty, full, mutex */

void* producer(void* arg) {
    int id = *(int*)arg;
    for (int i = 0; i < NUM_ITEMS; i++) {
        int item = id * 100 + i;
        
        /* TODO: 等待空缓冲区 */
        /* TODO: 互斥访问缓冲区 */
        
        buffer[in] = item;
        printf("生产者 %d: 生产 %d，放入位置 %d\n", id, item, in);
        in = (in + 1) % BUFFER_SIZE;
        
        /* TODO: 释放互斥锁 */
        /* TODO: 增加满缓冲区计数 */
        
        sleep(rand() % 2);
    }
    printf("生产者 %d 完成\n", id);
    return NULL;
}

void* consumer(void* arg) {
    int id = *(int*)arg;
    for (int i = 0; i < NUM_ITEMS; i++) {
        /* TODO: 等待满缓冲区 */
        /* TODO: 互斥访问缓冲区 */
        
        int item = buffer[out];
        printf("消费者 %d: 消费 %d，从位置 %d\n", id, item, out);
        out = (out + 1) % BUFFER_SIZE;
        
        /* TODO: 释放互斥锁 */
        /* TODO: 增加空缓冲区计数 */
        
        sleep(rand() % 2);
    }
    printf("消费者 %d 完成\n", id);
    return NULL;
}

int main() {
    srand(time(NULL));
    
    /* TODO: 初始化信号量 */
    
    pthread_t prod[2], cons[2];
    int ids[2] = {0, 1};
    
    /* TODO: 创建生产者和消费者线程 */
    
    /* TODO: 等待所有线程结束 */
    
    /* TODO: 销毁信号量 */
    
    printf("所有生产者消费者完成\n");
    return 0;
}