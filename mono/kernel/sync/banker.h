#ifndef KERNEL_SYNC_BANKER_H
#define KERNEL_SYNC_BANKER_H

#define BANKER_MAX_PROC 8
#define BANKER_MAX_RES 4

struct banker_state {
    int nproc;
    int nres;
    int available[BANKER_MAX_RES];
    int max[BANKER_MAX_PROC][BANKER_MAX_RES];
    int allocation[BANKER_MAX_PROC][BANKER_MAX_RES];
    int need[BANKER_MAX_PROC][BANKER_MAX_RES];
};

/* 返回安全序列长度；safe_seq 写入进程下标。不安全返回 0。 */
int banker_safety_check(const struct banker_state *state, int *safe_seq);

/* 用王道 2.4 经典矩阵做自检并打印。 */
void banker_selftest(void);

#endif
