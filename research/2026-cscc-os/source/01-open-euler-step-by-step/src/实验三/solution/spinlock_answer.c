/*
 * solution/spinlock_answer.c - 自旋锁（完整答案）
 * 功能：实现自旋锁，对比有锁和无锁的差异
 */

#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>

#define NUM_THREADS 4
#define ITERATIONS 100000

typedef struct {
    atomic_flag lock;
} spinlock_t;

void spinlock_init(spinlock_t *sl) {
    atomic_flag_clear(&sl->lock);
}

void spinlock_lock(spinlock_t *sl) {
    while (atomic_flag_test_and_set(&sl->lock)) {
        // 自旋等待
    }
}

void spinlock_unlock(spinlock_t *sl) {
    atomic_flag_clear(&sl->lock);
}

long long counter = 0;
spinlock_t spinlock;

void* thread_with_lock(void* arg) {
    int id = *(int*)arg;
    for (int i = 0; i < ITERATIONS; i++) {
        spinlock_lock(&spinlock);
        counter++;
        spinlock_unlock(&spinlock);
    }
    printf("线程 %d 执行完毕\n", id);
    return NULL;
}

void* thread_no_lock(void* arg) {
    int id = *(int*)arg;
    for (int i = 0; i < ITERATIONS; i++) {
        counter++;
    }
    printf("线程 %d (无锁) 执行完毕\n", id);
    return NULL;
}

int main() {
    pthread_t threads[NUM_THREADS];
    int ids[NUM_THREADS];
    
    counter = 0;
    printf("=== 无锁测试 ===\n");
    for (int i = 0; i < NUM_THREADS; i++) {
        ids[i] = i;
        pthread_create(&threads[i], NULL, thread_no_lock, &ids[i]);
    }
    for (int i = 0; i < NUM_THREADS; i++) {
        pthread_join(threads[i], NULL);
    }
    printf("预期: %d, 实际: %lld (数据竞争导致错误)\n\n", NUM_THREADS * ITERATIONS, counter);
    
    counter = 0;
    spinlock_init(&spinlock);
    printf("=== 自旋锁测试 ===\n");
    for (int i = 0; i < NUM_THREADS; i++) {
        ids[i] = i;
        pthread_create(&threads[i], NULL, thread_with_lock, &ids[i]);
    }
    for (int i = 0; i < NUM_THREADS; i++) {
        pthread_join(threads[i], NULL);
    }
    printf("预期: %d, 实际: %lld (结果正确)\n", NUM_THREADS * ITERATIONS, counter);
    
    return 0;
}