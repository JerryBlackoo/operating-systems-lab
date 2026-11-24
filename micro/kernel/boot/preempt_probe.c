/*
 * preempt_probe.c — 默认启动路径上的定时器抢占冒烟测试。
 *
 * 只在 BOOT_SELECTED == capability 的镜像里链接。它不是某个模块 demo
 * 的平行世界：清单跑完后，bootstrap 用正式 sched/clock API 创建两条
 * 内核线程、武装 1 ms tick，并让 timer 中断经 shared/demo.c 的统一
 * trap glue 触发 sched_tick()/sched_check()。
 *
 * 判别性证据：
 *   - worker-b 在 worker-a 完成前获得 CPU（overlap）；
 *   - worker-a / worker-b 都在时间片到期后被切走再恢复（resumed）；
 *   - tick / quantum / switch 只是辅助计数，单独不能证明抢占。
 *
 * 这是默认启动路径上的验证专用例外；boot 自身仍不创建首个服务线程。
 */
#include <stdbool.h>
#include <stdint.h>

#include "clock/clock.h"
#include "csr.h"
#include "demo.h"
#include "object.h"
#include "sched.h"

#define PROBE_TICK_NS     1000000ULL /* 1 ms */
#define PROBE_WORK_TICKS  150000ULL  /* 10 MHz timebase 下 15 ms */
#define PROBE_SLEEP_NS    60000000ULL

static _Alignas(16) uint8_t worker_a_stack[4096];
static _Alignas(16) uint8_t worker_b_stack[4096];

/* probe 自己的 bootstrap 线程；不能复用任何模块 run() 的栈局部 current。 */
static struct thread probe_runner;

struct probe_state {
    volatile unsigned a_started;
    volatile unsigned a_done;
    volatile unsigned b_started;
    volatile unsigned b_done;
    volatile unsigned a_resumed;
    volatile unsigned b_resumed;
    volatile unsigned overlap;
};

static struct probe_state probe;

/* 日志期间关中断，避免另一条 worker 把半行输出插进来。 */
static void probe_log(const char *line)
{
    uint64_t flags = irq_save();

    demo_log(line);
    irq_restore(flags);
}

static char *put_str(char *p, const char *s)
{
    while (*s)
        *p++ = *s++;
    return p;
}

static char *put_dec(char *p, uint64_t v)
{
    char tmp[24];
    unsigned n = 0;

    if (v == 0) {
        *p++ = '0';
        return p;
    }
    while (v != 0) {
        tmp[n++] = (char)('0' + v % 10);
        v /= 10;
    }
    while (n != 0)
        *p++ = tmp[--n];
    return p;
}

static void spin_work(void)
{
    clock_ticks_t start = clock_ticks();

    while (clock_ticks() - start < PROBE_WORK_TICKS) {
        for (volatile unsigned i = 0; i < 1000; i++) {
        }
    }
}

static void worker_a(void *argument)
{
    uint64_t switches = sched_switch_count();

    (void)argument;
    probe.a_started = 1;
    probe_log("[bootstrap] preempt: worker-a starts without yielding");
    spin_work();
    probe.a_done = 1;
    if (sched_switch_count() != switches) {
        probe.a_resumed = 1;
        probe_log("[bootstrap] preempt: worker-a resumed after timer switch");
    }
}

static void worker_b(void *argument)
{
    uint64_t switches = sched_switch_count();

    (void)argument;
    probe.b_started = 1;
    if (!probe.a_done) {
        probe.overlap = 1;
        probe_log("[bootstrap] preempt: worker-b starts before worker-a done");
    } else {
        probe_log("[bootstrap] preempt: worker-b starts after worker-a done "
                  "(no overlap)");
    }
    spin_work();
    probe.b_done = 1;
    if (sched_switch_count() != switches) {
        probe.b_resumed = 1;
        probe_log("[bootstrap] preempt: worker-b resumed after timer switch");
    }
}

void boot_preempt_probe(void)
{
    uint64_t switches_at_arm;
    uint64_t ticks;
    uint64_t switches;
    uint64_t quantum;
    char buf[192];
    char *p = buf;

    /*
     * capability 模块的 current_thread 是它 run() 栈上的局部 runner，
     * 模块返回后已经失效；不能用“sched_current()!=NULL”推断仍有合法
     * bootstrap 线程。capability 的断言此时已完成，这里在自己的静态
     * probe_runner 上重建对象/调度状态，再交还一个合法 current。
     */
    object_system_init();
    sched_init();
    probe_runner = (struct thread){.name = "boot-preempt"};
    sched_bootstrap(&probe_runner);

    clock_init();
    CHECK(thread_create(NULL, worker_a, NULL, worker_a_stack,
                        sizeof(worker_a_stack), "preempt-a") != NULL);
    CHECK(thread_create(NULL, worker_b, NULL, worker_b_stack,
                        sizeof(worker_b_stack), "preempt-b") != NULL);

    CHECK(sched_start_tick(PROBE_TICK_NS));
    CHECK((csr_read(CSR_SIE) & SIE_STIE) != 0);
    demo_log("[bootstrap] preempt: 1 ms SBI timer armed on unified trap path");

    switches_at_arm = sched_switch_count();
    irq_enable();

    /* 主线程睡足 60 ms，让两条 worker 在 timer 中断下交替运行。 */
    thread_sleep_ns(PROBE_SLEEP_NS);

    irq_disable();
    sched_stop_tick();

    ticks = sched_tick_count();
    switches = sched_switch_count();
    quantum = sched_quantum_expiration_count();

    p = put_str(p, "[bootstrap] preempt: ticks=");
    p = put_dec(p, ticks);
    p = put_str(p, " switches=");
    p = put_dec(p, switches - switches_at_arm);
    p = put_str(p, " quantum_expired=");
    p = put_dec(p, quantum);
    p = put_str(p, " workers=2");
    *p = '\0';
    demo_log(buf);

    CHECK(probe.a_started && probe.b_started);
    CHECK(probe.a_done && probe.b_done);
    CHECK(probe.overlap);
    CHECK(probe.a_resumed && probe.b_resumed);
    CHECK(ticks >= 2);
    CHECK(quantum >= 1);
    CHECK(switches - switches_at_arm >= 2);
}