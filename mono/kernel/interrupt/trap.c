#include "arch/riscv/trap.h"

#include "arch/riscv/csr.h"
#include "clock/clock.h"
#include "console.h"
#include "exception/exception.h"
#include "mm/trap_vm.h"
#include "proc/proc.h"
#include "syscall/syscall.h"

#define SCAUSE_INTERRUPT (1UL << 63)
#define INTERRUPT_S_TIMER 5UL

void trap_init(void)
{
    csr_write(stvec, (uint64_t)trap_entry);
    csr_set(sie, SIE_STIE);
    csr_write(sscratch, 0);

    console_write("[trap] stvec installed at ");
    console_write_hex((uint64_t)trap_entry);
    console_write("\n");
}

void trap_enable(void)
{
    csr_set(sstatus, SSTATUS_SIE);
}

/* U-mode 的非法指令/断点/未对齐/访问错误属于用户程序错误，不能停机
 * 整机；复用缺页杀进程路径，回收 fd 与用户地址空间后让出 CPU。 */
static void kill_current_user(uint64_t cause, uint64_t epc)
    __attribute__((noreturn));

static void kill_current_user(uint64_t cause, uint64_t epc)
{
    console_write("[exception] kill pid=");
    console_write_dec((uint64_t)current->pid);
    console_write(" name=");
    console_write(current->name);
    console_write(" sepc=");
    console_write_hex(epc);
    console_write(" scause=");
    console_write_hex(cause);
    console_write("\n");

    current->exit_code = -1;
    proc_exit_cleanup();
    current->state = PROC_ZOMBIE;
    vm_stats_print();
    for (;;) {
        yield();
    }
}

void trap_dispatch(struct trap_frame *tf)
{
    uint64_t scause = csr_read(scause);
    uint64_t sepc = csr_read(sepc);
    uint64_t stval = csr_read(stval);

    /* arch_init() 在 proc_init() 之前安装向量；启动阶段异常时 current 为空。 */
    if (current != 0) {
        current->tf = tf;
    }

    if ((scause & SCAUSE_INTERRUPT) != 0) {
        uint64_t code = scause & ~SCAUSE_INTERRUPT;

        if (code == INTERRUPT_S_TIMER) {
            clock_handle_interrupt();
            if (sched_need_resched() != 0) {
                sched_clear_need_resched();
                yield();
            }
            return;
        }

        console_write("[trap] unexpected interrupt, scause=");
        console_write_hex(scause);
        console_write(" sepc=");
        console_write_hex(sepc);
        console_write("\n");
        return;
    }

    if (scause == 8 || scause == 9) {
        tf->sepc += 4;
        tf->x[10] = (uint64_t)syscall_dispatch(tf->x[17], tf->x[10], tf->x[11], tf->x[12]);
        return;
    }

    if (scause == 12 || scause == 13 || scause == 15) {
        /* 来自 S-mode 的页故障是内核 bug；即使 current 是用户进程，
         * 也不能先交给用户缺页处理，否则可能被静默映射后重试。 */
        if ((tf->sstatus & SSTATUS_SPP) != 0) {
            exception_handle(scause, sepc, stval, tf);
        }
        if (vm_handle_fault(stval, scause == 15, scause == 12) != 0) {
            return;
        }
        console_write("[vm] kill pid=");
        console_write_dec((uint64_t)current->pid);
        console_write(" va=");
        console_write_hex(stval);
        console_write("\n");
        current->exit_code = -1;
        proc_exit_cleanup();
        current->state = PROC_ZOMBIE;
        vm_stats_print();
        yield();
        return;
    }

    /* 非缺页的 U-mode 异常（非法指令、ebreak、未对齐、访问错误等）
     * 只杀当前进程；S-mode 异常才视为内核 bug 停机。 */
    if (current != 0 && current->is_user != 0 &&
        (tf->sstatus & SSTATUS_SPP) == 0) {
        kill_current_user(scause, sepc);
    }
    exception_handle(scause, sepc, stval, tf);
}
