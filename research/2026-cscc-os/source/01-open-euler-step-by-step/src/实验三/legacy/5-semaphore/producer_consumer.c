#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <semaphore.h>
#include <unistd.h>

#define BUFFER_SIZE 5
#define NUM_ITEMS 10

int buffer[BUFFER_SIZE];
int in = 0, out = 0;
sem_t empty, full, mutex;

void* producer(void* arg) {
    int id = *(int*)arg;
    for (int i = 0; i < NUM_ITEMS; i++) {
        int item = id * 100 + i;
        
        // TODO: 等待空缓冲区
        sem_wait(&empty);
        // TODO: 互斥访问缓冲区
        sem_wait(&mutex);
        
        // TODO: 生产数据放入缓冲区
        buffer[in] = item;
        printf("生产者 %d: 生产 %d，放入位置 %d\n", id, item, in);
        in = (in + 1) % BUFFER_SIZE;
        
        // TODO: 释放互斥锁
        sem_post(&mutex);
        // TODO: 增加满缓冲区计数
        sem_post(&full);
        
        sleep(rand() % 2);
    }
    printf("生产者 %d 完成\n", id);
    return NULL;
}

void* consumer(void* arg) {
    int id = *(int*)arg;
    for (int i = 0; i < NUM_ITEMS; i++) {
        // TODO: 等待满缓冲区
        sem_wait(&full);
        // TODO: 互斥访问缓冲区
        sem_wait(&mutex);
        
        // TODO: 从缓冲区消费数据
        int item = buffer[out];
        printf("消费者 %d: 消费 %d，从位置 %d\n", id, item, out);
        out = (out + 1) % BUFFER_SIZE;
        
        // TODO: 释放互斥锁
        sem_post(&mutex);
        // TODO: 增加空缓冲区计数
        sem_post(&empty);
        
        sleep(rand() % 2);
    }
    printf("消费者 %d 完成\n", id);
    return NULL;
}

int main() {
    srand(time(NULL));
    
    // TODO: 初始化信号量
    sem_init(&empty, 0, BUFFER_SIZE);
    sem_init(&full, 0, 0);
    sem_init(&mutex, 0, 1);
    
    pthread_t prod[2], cons[2];
    int ids[2] = {0, 1};
    
    // TODO: 创建生产者和消费者线程
    pthread_create(&prod[0], NULL, producer, &ids[0]);
    pthread_create(&prod[1], NULL, producer, &ids[1]);
    pthread_create(&cons[0], NULL, consumer, &ids[0]);
    pthread_create(&cons[1], NULL, consumer, &ids[1]);
    
    // TODO: 等待所有线程结束
    for (int i = 0; i < 2; i++) {
        pthread_join(prod[i], NULL);
        pthread_join(cons[i], NULL);
    }
    
    // TODO: 销毁信号量
    sem_destroy(&empty);
    sem_destroy(&full);
    sem_destroy(&mutex);
    
    printf("所有生产者消费者完成\n");
    return 0;
}