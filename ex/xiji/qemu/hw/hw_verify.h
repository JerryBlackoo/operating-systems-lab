#ifndef XIJI_HW_VERIFY_H
#define XIJI_HW_VERIFY_H

/*
 * QEMU 硬件轨：每一项都直接驱动真实设备（CMOS / 8259 / 8254 / CR0.PG+#PF /
 * ATA PIO），并与 src/ 里的实验算法共享同一份实现。
 *
 * 返回 0 表示全部通过；任何一项失败会打印 "FAIL <name>: <reason>" 并返回非 0。
 */
int hw_verify_all(void);

#endif
