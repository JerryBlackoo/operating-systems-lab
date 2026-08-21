/*
 * starter/spinlock.c - 自旋锁（学生填空版）
 * TODO: 补全代码，实现自旋锁并对比有无锁的差异
 */

#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>

#define NUM_THREADS 4
#define ITERATIONS 100000

/* TODO: 定义自旋锁结构体 */
/* ┌─ 请在此处补全代码 ─┐ */

/* └───────────────────┘ */

/* TODO: 初始化自旋锁 */
void spinlock_init(/* TODO: 参数 */) {
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
}

/* TODO: 自旋锁加锁（忙等待） */
void spinlock_lock(/* TODO: 参数 */) {
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
}

/* TODO: 自旋锁解锁 */
void spinlock_unlock(/* TODO: 参数 */) {
    /* ┌─ 请在此处补全代码 ─┐ */
    
    /* └───────────────────┘ */
}

long long counter = 0;
/* TODO: 声明自旋锁变量 */

void* thread_with_lock(void* arg) {
    int id = *(int*)arg;
    for (int i = 0; i < ITERATIONS; i++) {
        /* TODO: 使用自旋锁保护临界区 */
        /* ┌─ 请在此处补全代码 ─┐ */
        counter++;
        /* └───────────────────┘ */
    }
    printf("线程 %d 执行完毕\n", id);
    return NULL;
}

void* thread_no_lock(void* arg) {
    int id = *(int*)arg;
    for (int i = 0; i < ITERATIONS; i++) {
        /* TODO: 无锁保护，产生数据竞争 */
        counter++;
    }
    printf("线程 %d (无锁) 执行完毕\n", id);
    return NULL;
}

int main() {
    pthread_t threads[NUM_THREADS];
    int ids[NUM_THREADS];
    
    /* TODO: 测试无锁版本 */
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
    
    /* TODO: 测试自旋锁版本 */
    counter = 0;
    /* TODO: 初始化自旋锁 */
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