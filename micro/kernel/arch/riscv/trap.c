/*
 * trap.c — 唯一接收 trap 的 C 函数。
 *
 * 契约（本模块最重要的一条）：
 *   struct trap_frame *trap_handler(struct trap_frame *tf);
 *   返回值 = 「下一个要跑的 frame」，不一定等于入参。
 *
 * 为什么不是 void：微内核里 trap 处理完可能这个线程不再可运行
 * （比如 IPC 阻塞、时间片到期），必须允许换成另一个线程的 frame。
 * Linux 的 ret_from_exception 隐含「回到 current」，微内核不能这么假设。
 *
 * 因此本函数返回后，trap.S 走 trap_return(返回值) ——
 * 「trap 返回」与「上下文切换」在汇编层是同一条路径。
 */
#include "trap.h"
#include "csr.h"
#include "hart.h"
#include "sbi.h"

/* ---- 最小输出：panic 用。正式 console 归 drivers/ ---- */
static void puts_raw(const char *s)
{
    while (*s)
        sbi_putchar(*s++);
}

static void put_hex_raw(u64 v)
{
    static const char d[] = "0123456789abcdef";
    puts_raw("0x");
    for (int i = 60; i >= 0; i -= 4)
        sbi_putchar(d[(v >> i) & 0xf]);
}

__attribute__((noreturn)) void panic(const char *why,
                                     const struct trap_frame *tf)
{
    puts_raw("\n[panic] ");
    puts_raw(why);

    if (tf) {
        puts_raw("\n        hart=");
        put_hex_raw(hart_local_get()->hartid);
        puts_raw("  sepc=");
        put_hex_raw(tf->sepc);
        puts_raw("\n        scause=");
        put_hex_raw(tf->scause);
        puts_raw("  stval=");
        put_hex_raw(tf->stval);
        puts_raw("  sstatus=");
        put_hex_raw(tf->sstatus);
    }
    puts_raw("\n[panic] kernel stopped\n");

    sbi_shutdown();
}

/* ---- 分发 ---------------------------------------------------------- */

static const char *exc_name(u64 cause)
{
    switch (cause) {
    case EXC_INST_MISALIGNED:  return "instruction address misaligned";
    case EXC_INST_ACCESS:      return "instruction access fault";
    case EXC_INST_ILLEGAL:     return "illegal instruction";
    case EXC_BREAKPOINT:       return "breakpoint";
    case EXC_LOAD_MISALIGNED:  return "load address misaligned";
    case EXC_LOAD_ACCESS:      return "load access fault";
    case EXC_STORE_MISALIGNED: return "store address misaligned";
    case EXC_STORE_ACCESS:     return "store access fault";
    case EXC_ECALL_U:          return "ecall from U-mode";
    case EXC_ECALL_S:          return "ecall from S-mode (OpenSBI missing?)";
    case EXC_INST_PAGEFAULT:   return "instruction page fault";
    case EXC_LOAD_PAGEFAULT:   return "load page fault";
    case EXC_STORE_PAGEFAULT:  return "store page fault";
    default:                   return "unknown exception";
    }
}

struct trap_frame *trap_handler(struct trap_frame *tf)
{
    u64 cause = tf->scause;
    int from_user = (tf->sstatus & SSTATUS_SPP) == 0;

    /* ---- 中断 ---- */
    if (cause & SCAUSE_IRQ) {
        switch (cause & ~SCAUSE_IRQ) {
        case IRQ_S_TIMER:
            /* arch 只说「时间到了」。是时间片还是 sleep 唤醒，归 clock/sched。 */
            return tf;
        case IRQ_S_EXT:
            /* 将来：PLIC claim/complete。最小模型里不会到这。 */
            return tf;
        case IRQ_S_SOFT:
            return tf;
        default:
            panic("unexpected interrupt", tf);
        }
    }

    /* ---- 异常 ---- */
    switch (cause) {
    case EXC_ECALL_U:
        /* 系统调用：将来交给 syscall 分发。最小模型里直接原样返回。 */
        if (!from_user)
            panic("ecall from S-mode reached kernel", tf);
        tf->sepc += 4;             /* 跳过 ecall 指令 */
        return tf;

    case EXC_INST_PAGEFAULT:
    case EXC_LOAD_PAGEFAULT:
    case EXC_STORE_PAGEFAULT:
        /* 归 memory 模块。最小模型：不处理，停。 */
        panic("page fault (MMU not enabled in minimal model)", tf);

    case EXC_INST_ILLEGAL:
#ifdef ARCH_SELFTEST
        /*
         * 仅架构自检镜像启用（ARCH_SELFTEST 由 arch/riscv/Makefile 定义）。
         * 自检故意写只读 CSR 制造非法指令，需要从故障指令之后继续，
         * 才能证明 trap 三件套（保存 → 分发 → 返回）真的通了。
         * 前提：故障指令是 4 字节非压缩编码；selftest.c 用 .option norvc
         * 保证这一点。正式内核路径不跳过非法指令，落到下方 panic。
         */
        if (!from_user) {
            puts_raw("[trap] illegal instruction caught in S-mode: scause=");
            put_hex_raw(cause);
            puts_raw(" sepc=");
            put_hex_raw(tf->sepc);
            puts_raw(" stval=");
            put_hex_raw(tf->stval);
            puts_raw(", resuming\n");
            tf->sepc += 4;
            return tf;
        }
#endif
        panic(exc_name(cause), tf);

    case EXC_ECALL_S:
        /* S-mode 的 ecall 应该被 M-mode 的 OpenSBI 截获。到这说明固件没接住。 */
        panic(exc_name(cause), tf);

    default:
        panic(exc_name(cause), tf);
    }
}
