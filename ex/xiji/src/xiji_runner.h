#ifndef XIJI_RUNNER_H
#define XIJI_RUNNER_H

/*
 * 七个希冀实验的统一入口。
 * 每个 xiji_expNN_run() 自成一体：先重置自己的状态，再跑断言，
 * 因此可以在 host 与 QEMU 上以任意顺序重复调用。
 */

int xiji_exp01_run(void);
int xiji_exp02_run(void);
int xiji_exp03_run(void);
int xiji_exp04_run(void);
int xiji_exp05_run(void);
int xiji_exp06_run(void);
int xiji_exp07_run(void);

/* ---- exp03 对外探针：QEMU 硬件轨用真实物理内存验证分区互不重叠 ---- */
enum {
    XIJI_FIT_FIRST = 0,
    XIJI_FIT_BEST = 1,
    XIJI_FIT_WORST = 2,
};

void xiji_exp03_reset(void);
int xiji_exp03_alloc(int policy, int size, int owner);
int xiji_exp03_free(int base);
int xiji_exp03_free_bytes(void);
int xiji_exp03_block_count(void);
int xiji_exp03_invariants_ok(void);

/* ---- exp07 对外探针：把目录树序列化成文本，供写入 ATA 扇区后回读比对 ---- */
void xiji_exp07_seed_demo(void);
int xiji_exp07_format(char *out, int cap);

#endif
