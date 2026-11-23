/*
 * csr.h — 本模块唯一允许出现 "csrr/csrw/csrs/csrc" 的地方。
 *
 * 为什么单独抽出来：
 *   1. 其它代码不该拼 CSR 指令字符串，拼错了是静默错误（汇编器不一定报）。
 *   2. 关中断必须是「一条 csrrc」，不能写成「读一行 + 改一行」。
 *
 * 编号本身携带权限，见文件末尾的说明。
 */
#ifndef MICRO_ARCH_RISCV_CSR_H
#define MICRO_ARCH_RISCV_CSR_H

#include "types.h"

/* ---- CSR 编号（只列本模块用到的） ---------------------------------- */
#define CSR_SSTATUS   0x100
#define CSR_SIE       0x104
#define CSR_STVEC     0x105
#define CSR_SSCRATCH  0x140
#define CSR_SEPC      0x141
#define CSR_SCAUSE    0x142
#define CSR_STVAL     0x143
#define CSR_SIP       0x144
#define CSR_SATP      0x180
#define CSR_TIME      0xc01   /* 只读，[11:10]=11 表示硬件写 */

/* ---- sstatus 位 ---------------------------------------------------- */
#define SSTATUS_SIE   (1UL << 1)    /* 中断总开关 */
#define SSTATUS_SPIE  (1UL << 5)    /* 进入 trap 前的 SIE，sret 时写回 SIE */
#define SSTATUS_SPP   (1UL << 8)    /* 1 = trap 来自 S-mode，0 = 来自 U-mode */
#define SSTATUS_SUM   (1UL << 18)   /* 允许 S-mode 读写 U=1 的页 */
#define SSTATUS_FS    (3UL << 13)
#define SSTATUS_FS_OFF (0UL << 13)

/* ---- sie / sip 位（异常码编号同 scause 低位） ---------------------- */
#define SIE_SSIE      (1UL << 1)    /* S 软件中断（未来 IPI） */
#define SIE_STIE      (1UL << 5)    /* S 定时器 */
#define SIE_SEIE      (1UL << 9)    /* S 外部（PLIC） */

/* ---- scause 高位：中断标志 ---------------------------------------- */
#define SCAUSE_IRQ    (1UL << 63)

/* ---- scause 异常码 ------------------------------------------------- */
#define EXC_INST_MISALIGNED 0
#define EXC_INST_ACCESS     1
#define EXC_INST_ILLEGAL    2
#define EXC_BREAKPOINT      3
#define EXC_LOAD_MISALIGNED 4
#define EXC_LOAD_ACCESS     5
#define EXC_STORE_MISALIGNED 6
#define EXC_STORE_ACCESS    7
#define EXC_ECALL_U         8
#define EXC_ECALL_S         9
#define EXC_INST_PAGEFAULT  12
#define EXC_LOAD_PAGEFAULT  13
#define EXC_STORE_PAGEFAULT 15

/* ---- scause 中断码 ------------------------------------------------- */
#define IRQ_S_SOFT   1
#define IRQ_S_TIMER  5
#define IRQ_S_EXT    9

/* ---- 六条 CSR 指令 + 四个别名 --------------------------------------
 *
 * 为什么用字符串化而不是 "i" 约束：
 *   GCC/Clang 的 CSR 名不是通用立即数约束能表达的（0x100UL 带后缀会让
 *   汇编器困惑），Linux 的做法是把编号拼进指令文本。照抄这个做法。
 *
 * 为什么每个都带 "memory" clobber：
 *   部分 CSR 的读/写有副作用或与前后指令有顺序要求，编译器无法区分。
 *   代价可忽略（CSR 访问不在热循环里），换来不踩顺序坑。
 */
#define _CSR_STR(x)  #x
#define _CSR_XSTR(x) _CSR_STR(x)

#define csr_read(csr)                                   \
    ({                                                  \
        u64 __v;                                        \
        __asm__ volatile("csrr %0, " _CSR_XSTR(csr)     \
                         : "=r"(__v)                    \
                         :                              \
                         : "memory");                   \
        __v;                                            \
    })

#define csr_write(csr, val)                             \
    ({                                                  \
        u64 __v = (u64)(val);                           \
        __asm__ volatile("csrw " _CSR_XSTR(csr) ", %0"  \
                         :                              \
                         : "rK"(__v)                    \
                         : "memory");                   \
    })

#define csr_set(csr, mask)                              \
    ({                                                  \
        u64 __v = (u64)(mask);                          \
        __asm__ volatile("csrs " _CSR_XSTR(csr) ", %0"  \
                         :                              \
                         : "rK"(__v)                    \
                         : "memory");                   \
    })

#define csr_clear(csr, mask)                            \
    ({                                                  \
        u64 __v = (u64)(mask);                          \
        __asm__ volatile("csrc " _CSR_XSTR(csr) ", %0"  \
                         :                              \
                         : "rK"(__v)                    \
                         : "memory");                   \
    })

/* 原子「读旧值 + 清位」。irq_save 必须用它。 */
#define csr_read_clear(csr, mask)                       \
    ({                                                  \
        u64 __v = (u64)(mask);                          \
        __asm__ volatile("csrrc %0, " _CSR_XSTR(csr) ", %1" \
                         : "=r"(__v)                    \
                         : "rK"(__v)                    \
                         : "memory");                   \
        __v;                                            \
    })

/* 原子「读旧值 + 置位」。 */
#define csr_read_set(csr, mask)                         \
    ({                                                  \
        u64 __v = (u64)(mask);                          \
        __asm__ volatile("csrrs %0, " _CSR_XSTR(csr) ", %1" \
                         : "=r"(__v)                    \
                         : "rK"(__v)                    \
                         : "memory");                   \
        __v;                                            \
    })

/* ---- 中断屏蔽：整个内核只有这一个开关 ------------------------------
 *
 * 单 hart + trap 期间关中断  =>  irq_save/irq_restore 就是全局互斥锁。
 * 引入 SMP 后这条失效，所有同步点必须改成「先关中断再拿自旋锁」。
 */
static inline u64 irq_save(void)      { return csr_read_clear(CSR_SSTATUS, SSTATUS_SIE); }
static inline void irq_disable(void)  { csr_clear(CSR_SSTATUS, SSTATUS_SIE); }
static inline void irq_enable(void)   { csr_set(CSR_SSTATUS, SSTATUS_SIE); }

static inline void irq_restore(u64 flags)
{
    if (flags & SSTATUS_SIE)
        csr_set(CSR_SSTATUS, SSTATUS_SIE);
}

static inline int irq_enabled(void)
{
    return (csr_read(CSR_SSTATUS) & SSTATUS_SIE) != 0;
}

#endif /* MICRO_ARCH_RISCV_CSR_H */

/*
 * 编号里的权限（RISC-V 规范 2.2 / 9.1）：
 *
 *   [11:10] == 11  -> 只读（csr_write 会 illegal instruction）
 *   [9:8]          -> 能访问它的最低特权级：00 U / 01 S / 10 H / 11 M
 *
 *   sstatus 0x100 -> 00_01  可写，S-mode
 *   time    0xc01 -> 11_00  只读，U-mode 就能读
 *   mhartid 0xf14 -> 11_11  只读，只有 M-mode（所以 hartid 只能从 a0 接）
 */
