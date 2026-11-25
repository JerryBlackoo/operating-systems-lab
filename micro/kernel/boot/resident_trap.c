#include "abi/abi.h"
#include "console/console.h"
#include "csr.h"
#include "demo.h"
#include "memory.h"
#include "sbi.h"
#include "sched.h"
#include "syscall/user_backend.h"
#include "syscall.h"
#include "trap.h"

enum { DEMO_PRINT = 0x10000 };

_Static_assert(sizeof(struct syscall_call) <= sizeof(((struct thread *)0)->ucall_words),
               "thread ucall_words too small");

static void resident_print(const char *line)
{
    if (line == NULL) {
        return;
    }
    if (console_mmio_active()) {
        console_puts(line);
        console_putc('\n');
    } else {
        while (*line != '\0') {
            sbi_putchar(*line++);
        }
        sbi_putchar('\n');
    }
}

struct trap_frame *resident_user_trap(struct trap_frame *frame)
{
    enum syscall_phase phase;

    if (frame->scause != EXC_ECALL_U || (frame->sstatus & SSTATUS_SPP)) {
        return frame;
    }
    /*
     * 被请求终止的线程在进入内核的第一时间退出。RUNNING 线程无法被当场
     * 摘除（sched_thread_terminate 只置位），由这里兑现；异步 syscall 上
     * 的线程另在 syscall_wait_park 返回后退出。
     */
    {
        struct thread *cur = sched_current();

        if (cur != NULL && sched_thread_kill_pending(cur)) {
            thread_exit();
        }
    }
    if (frame->a7 == (uint64_t)DEMO_PRINT) {
        struct thread *cur = sched_current();
        char line[128];

        line[0] = '\0';
        if (cur != NULL && cur->as != NULL) {
            /*
             * as_copyin 走 direct map 的物理帧拷贝，不经过用户 VA，
             * 因此不需要 SSTATUS.SUM；在这里临时打开 SUM 会在 timer
             * 抢占、切换到别的 AS 时把 SUM 留在 sstatus 上。
             */
            if (as_copyin(cur->as, (vaddr_t)frame->a0, line, sizeof(line) - 1) ==
                MEM_OK) {
                line[sizeof(line) - 1] = '\0';
                resident_print(line);
            }
        }
        frame->sepc += 4;
        return frame;
    }

    {
        uint32_t caller = 0;
        struct thread *cur = sched_current();

        if (cur != NULL) {
            struct syscall_call *ucall = (struct syscall_call *)cur->ucall_words;

            caller = sched_thread_cap_sid(cur);
            phase = syscall_dispatch(ucall, frame, caller,
                                     &user_syscall_backend);
        } else {
            phase = SYSCALL_IGNORED;
        }
    }
    if (phase == SYSCALL_EXITED) {
        thread_exit();
    }
    if (phase == SYSCALL_WAITING) {
        struct thread *cur = sched_current();
        struct syscall_call *ucall = cur != 0 ?
            (struct syscall_call *)cur->ucall_words :
            0;

        /*
         * 后端返回 WAITING 时 sepc 已推进，但尚未写回 a0/a1…。
         * 在返回用户态前停泊，直到 syscall_resume 写完 trap 帧并唤醒。
         * resident 下 ABI_IPC_CALL 用 ep_call_submit + ep_reply 续完。
         */
        syscall_wait_park(ucall);
        return frame;
    }
    if (phase == SYSCALL_COMPLETE || phase == SYSCALL_EXITED) {
        return frame;
    }
    demo_fail("resident syscall phase", __LINE__);
    return frame;
}