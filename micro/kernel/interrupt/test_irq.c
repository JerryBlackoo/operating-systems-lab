#include "irq.h"

#include <stdbool.h>
#include <stdio.h>
#include <string.h>

#define WORDS ((IRQ_LINE_MAX + 31) / 32)

static int g_failures;

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);    \
            g_failures++;                                                      \
        }                                                                      \
    } while (0)

/*
 * 主机侧 Mock，按 QEMU virt PLIC 的关键语义建模：
 *   - 只有 enabled && pending && !claimed 的线才会被 claim；
 *   - claim 会清 pending、置 claimed，形成“闸门”；
 *   - complete 清 claimed；若电平仍为高且 enabled，则立即重新 pending；
 *   - 被 claim 期间的新事件不排队（PLIC 的 source 闸门语义）。
 */
struct mock_chip {
    uint32_t pending[WORDS];
    uint32_t enabled[WORDS];
    uint32_t claimed[WORDS];
    uint32_t level[WORDS];

    bool ints_on;
    unsigned int mask_calls;
    unsigned int unmask_calls;
    unsigned int complete_calls;
    unsigned int claim_calls;
    unsigned int timer_rearms;
    unsigned int save_calls;
    unsigned int restore_calls;
};

static struct mock_chip g_mock;

static bool bit_get(const uint32_t *bitmap, int line)
{
    return (bitmap[line / 32] & (1u << (line % 32))) != 0;
}

static void bit_set(uint32_t *bitmap, int line)
{
    bitmap[line / 32] |= 1u << (line % 32);
}

static void bit_clear(uint32_t *bitmap, int line)
{
    bitmap[line / 32] &= ~(1u << (line % 32));
}

static int mock_claim(void)
{
    g_mock.claim_calls++;

    for (int line = 1; line < IRQ_LINE_MAX; line++) {
        if (bit_get(g_mock.enabled, line) &&
            bit_get(g_mock.pending, line) &&
            !bit_get(g_mock.claimed, line)) {
            bit_clear(g_mock.pending, line);
            bit_set(g_mock.claimed, line);
            return line;
        }
    }

    return IRQ_NONE;
}

static void mock_complete(int line)
{
    g_mock.complete_calls++;

    if (line == IRQ_TIMER) {
        g_mock.timer_rearms++;
        return;
    }
    if (line < 1 || line >= IRQ_LINE_MAX) {
        return;
    }

    bit_clear(g_mock.claimed, line);

    /* 电平仍为高且未屏蔽：complete 之后立即重新 pending。 */
    if (bit_get(g_mock.level, line) && bit_get(g_mock.enabled, line)) {
        bit_set(g_mock.pending, line);
    }
}

static void mock_mask(int line)
{
    g_mock.mask_calls++;

    if (line < 1 || line >= IRQ_LINE_MAX) {
        return; /* timer 没有 PLIC enable 位 */
    }

    bit_clear(g_mock.enabled, line);
}

static void mock_unmask(int line)
{
    g_mock.unmask_calls++;

    if (line < 1 || line >= IRQ_LINE_MAX) {
        return; /* timer 由 sie.STIE 控制，本 Mock 不建模 */
    }

    bit_set(g_mock.enabled, line);

    if (bit_get(g_mock.level, line) && !bit_get(g_mock.claimed, line)) {
        bit_set(g_mock.pending, line);
    }
}

static unsigned long mock_intr_save(void)
{
    g_mock.save_calls++;

    unsigned long flags = g_mock.ints_on ? 1UL : 0UL;
    g_mock.ints_on = false;
    return flags;
}

static void mock_intr_restore(unsigned long flags)
{
    g_mock.restore_calls++;
    g_mock.ints_on = (flags != 0);
}

static const struct irq_port g_mock_port = {
    .claim = mock_claim,
    .complete = mock_complete,
    .mask = mock_mask,
    .unmask = mock_unmask,
    .intr_save = mock_intr_save,
    .intr_restore = mock_intr_restore,
};

static void mock_reset(void)
{
    memset(&g_mock, 0, sizeof(g_mock));
    g_mock.ints_on = true;
    CHECK(irq_init(&g_mock_port) == IRQ_OK);
}

static void mock_raise(int line)
{
    if (line < 1 || line >= IRQ_LINE_MAX) {
        return;
    }

    bit_set(g_mock.level, line);

    if (bit_get(g_mock.enabled, line) && !bit_get(g_mock.claimed, line)) {
        bit_set(g_mock.pending, line);
    }
}

static void mock_set_level(int line, bool asserted)
{
    if (line < 1 || line >= IRQ_LINE_MAX) {
        return;
    }

    if (asserted) {
        mock_raise(line);
    } else {
        bit_clear(g_mock.level, line);
    }
}

static void mock_clear_device(int line)
{
    mock_set_level(line, false);
}

static bool mock_enabled(int line)
{
    return line >= 0 && line < IRQ_LINE_MAX && bit_get(g_mock.enabled, line);
}

static bool mock_pending(int line)
{
    return line >= 0 && line < IRQ_LINE_MAX && bit_get(g_mock.pending, line);
}

static bool mock_claimed(int line)
{
    return line >= 0 && line < IRQ_LINE_MAX && bit_get(g_mock.claimed, line);
}

struct fake_sink {
    unsigned int notifies;
    int last_line;
};

static struct fake_sink g_sink;
static struct irq_sink g_sink_base;

static void fake_notify(struct irq_sink *self, int line)
{
    struct fake_sink *sink = self->ctx;

    sink->notifies++;
    sink->last_line = line;
}

static void fake_sink_reset(void)
{
    g_sink.notifies = 0;
    g_sink.last_line = -1;
    g_sink_base.notify = fake_notify;
    g_sink_base.ctx = &g_sink;
}

static enum irq_state state_of(int line)
{
    enum irq_state state = IRQ_DISARMED;

    CHECK(irq_get_state(line, &state) == IRQ_OK);
    return state;
}

static struct irq_stats stats_of(int line)
{
    struct irq_stats stats;

    memset(&stats, 0, sizeof(stats));
    CHECK(irq_get_stats(line, &stats) == IRQ_OK);
    return stats;
}

static void test_uninitialized(void)
{
    struct irq_stats stats;
    enum irq_state state;

    fake_sink_reset();

    CHECK(irq_init(NULL) == IRQ_EINVAL);
    CHECK(irq_bind(10, &g_sink_base) == IRQ_EINVAL);
    CHECK(irq_unbind(10) == IRQ_EINVAL);
    CHECK(irq_ack(10) == IRQ_EINVAL);
    CHECK(irq_fire(10) == IRQ_EINVAL);
    CHECK(irq_dispatch() == IRQ_EINVAL);
    CHECK(irq_get_state(10, &state) == IRQ_EINVAL);
    CHECK(irq_get_stats(10, &stats) == IRQ_EINVAL);
}

static void test_init_and_bind(void)
{
    mock_reset();
    fake_sink_reset();

    CHECK(g_mock.mask_calls == IRQ_LINE_MAX);
    CHECK(state_of(IRQ_TIMER) == IRQ_DISARMED);
    CHECK(state_of(10) == IRQ_DISARMED);
    CHECK(state_of(IRQ_LINE_MAX - 1) == IRQ_DISARMED);
    CHECK(!mock_enabled(10));

    CHECK(irq_bind(10, &g_sink_base) == IRQ_OK);
    CHECK(state_of(10) == IRQ_ARMED);
    CHECK(mock_enabled(10));
    CHECK(g_mock.unmask_calls == 1);

    CHECK(irq_bind(10, &g_sink_base) == IRQ_EBUSY);
    CHECK(irq_unbind(10) == IRQ_OK);
    CHECK(state_of(10) == IRQ_DISARMED);
    CHECK(!mock_enabled(10));
    CHECK(irq_unbind(10) == IRQ_ENOENT);
    CHECK(g_mock.save_calls == g_mock.restore_calls);
}

static void test_spurious(void)
{
    mock_reset();
    fake_sink_reset();

    CHECK(irq_fire(10) == 0);
    CHECK(g_sink.notifies == 0);
    CHECK(g_mock.complete_calls == 1);
    CHECK(stats_of(10).spurious == 1);
    CHECK(state_of(10) == IRQ_DISARMED);

    CHECK(irq_fire(IRQ_LINE_MAX) == IRQ_EINVAL);
    CHECK(irq_fire(-1) == IRQ_EINVAL);
}

static void test_one_shot_level(void)
{
    mock_reset();
    fake_sink_reset();
    CHECK(irq_bind(10, &g_sink_base) == IRQ_OK);

    mock_set_level(10, true);
    CHECK(irq_dispatch() == 1);
    CHECK(g_sink.notifies == 1);
    CHECK(g_sink.last_line == 10);
    CHECK(state_of(10) == IRQ_ACTIVE);
    CHECK(mock_claimed(10));
    CHECK(stats_of(10).fires == 1);

    /* ACTIVE 期间再次置位：控制器闸门合并，不产生第二次 notify。 */
    mock_set_level(10, true);
    CHECK(irq_dispatch() == 0);
    CHECK(g_sink.notifies == 1);

    /* 直接模拟“控制器重复 claim”，核心必须只计数、不 notify、不 complete。 */
    CHECK(irq_fire(10) == 0);
    CHECK(stats_of(10).coalesced == 1);
    CHECK(g_sink.notifies == 1);
    CHECK(state_of(10) == IRQ_ACTIVE);

    /* 未清设备就 ack：电平仍高，complete 后立即重新 pending。 */
    CHECK(irq_ack(10) == IRQ_OK);
    CHECK(state_of(10) == IRQ_ARMED);
    CHECK(!mock_claimed(10));
    CHECK(mock_pending(10));
    CHECK(stats_of(10).acks == 1);

    CHECK(irq_dispatch() == 1);
    CHECK(g_sink.notifies == 2);
    CHECK(state_of(10) == IRQ_ACTIVE);

    /* 先清设备再 ack：complete 后不再 pending。 */
    mock_clear_device(10);
    CHECK(!mock_pending(10));
    CHECK(irq_ack(10) == IRQ_OK);
    CHECK(!mock_pending(10));
    CHECK(irq_dispatch() == 0);
    CHECK(g_sink.notifies == 2);

    /* ARMED 状态的 ack 是错误，必须计数。 */
    CHECK(irq_ack(10) == IRQ_EINVAL);
    CHECK(stats_of(10).bad_acks == 1);
}

static void test_unbind_active(void)
{
    mock_reset();
    fake_sink_reset();
    CHECK(irq_bind(10, &g_sink_base) == IRQ_OK);

    mock_set_level(10, true);
    CHECK(irq_dispatch() == 1);
    CHECK(state_of(10) == IRQ_ACTIVE);
    CHECK(g_sink.notifies == 1);

    CHECK(irq_unbind(10) == IRQ_OK);
    CHECK(state_of(10) == IRQ_DISARMED);
    CHECK(!mock_enabled(10));
    CHECK(!mock_claimed(10));
    CHECK(!mock_pending(10));
    CHECK(irq_dispatch() == 0);
    CHECK(g_sink.notifies == 1);
    CHECK(stats_of(10).fires == 1);
    CHECK(stats_of(10).acks == 0);

    /* 重新绑定：设备源仍为高，unmask 应立即补一个 pending。 */
    CHECK(irq_bind(10, &g_sink_base) == IRQ_OK);
    CHECK(mock_pending(10));
    CHECK(irq_dispatch() == 1);
    CHECK(g_sink.notifies == 2);

    mock_clear_device(10);
    CHECK(irq_ack(10) == IRQ_OK);
}

static void test_timer(void)
{
    mock_reset();
    fake_sink_reset();
    CHECK(irq_bind(IRQ_TIMER, &g_sink_base) == IRQ_OK);

    CHECK(irq_fire(IRQ_TIMER) == 1);
    CHECK(g_sink.notifies == 1);
    CHECK(g_sink.last_line == IRQ_TIMER);
    CHECK(state_of(IRQ_TIMER) == IRQ_ACTIVE);
    CHECK(stats_of(IRQ_TIMER).fires == 1);

    CHECK(irq_fire(IRQ_TIMER) == 0);
    CHECK(stats_of(IRQ_TIMER).coalesced == 1);

    CHECK(irq_ack(IRQ_TIMER) == IRQ_OK);
    CHECK(state_of(IRQ_TIMER) == IRQ_ARMED);
    CHECK(g_mock.timer_rearms == 1);

    CHECK(irq_fire(IRQ_TIMER) == 1);
    CHECK(g_sink.notifies == 2);
    CHECK(irq_ack(IRQ_TIMER) == IRQ_OK);
    CHECK(g_mock.timer_rearms == 2);

    CHECK(irq_unbind(IRQ_TIMER) == IRQ_OK);
    CHECK(g_mock.timer_rearms == 2);

    /* 未绑定的 timer 事件：计数并重装，避免时钟永久停摆。 */
    CHECK(irq_fire(IRQ_TIMER) == 0);
    CHECK(stats_of(IRQ_TIMER).spurious == 1);
    CHECK(g_mock.timer_rearms == 3);
}

static void test_multi_source_budget(void)
{
    mock_reset();
    fake_sink_reset();

    for (int line = 1; line <= 40; line++) {
        CHECK(irq_bind(line, &g_sink_base) == IRQ_OK);
        mock_set_level(line, true);
    }

    CHECK(irq_dispatch() == IRQ_DRAIN_BUDGET);
    CHECK(g_sink.notifies == IRQ_DRAIN_BUDGET);
    CHECK(state_of(1) == IRQ_ACTIVE);
    CHECK(state_of(33) == IRQ_ARMED);

    CHECK(irq_dispatch() == 40 - IRQ_DRAIN_BUDGET);
    CHECK(g_sink.notifies == 40);
    CHECK(state_of(40) == IRQ_ACTIVE);

    for (int line = 1; line <= 40; line++) {
        mock_clear_device(line);
        CHECK(irq_ack(line) == IRQ_OK);
        CHECK(state_of(line) == IRQ_ARMED);
    }

    CHECK(stats_of(40).acks == 1);
    CHECK(g_mock.save_calls == g_mock.restore_calls);
}

static void test_invalid(void)
{
    struct irq_stats stats;
    enum irq_state state;
    struct irq_sink bad;

    mock_reset();
    fake_sink_reset();
    memset(&bad, 0, sizeof(bad));

    CHECK(irq_ack(10) == IRQ_EINVAL);
    CHECK(stats_of(10).bad_acks == 1);
    CHECK(irq_ack(IRQ_LINE_MAX) == IRQ_EINVAL);
    CHECK(irq_bind(IRQ_LINE_MAX, &g_sink_base) == IRQ_EINVAL);
    CHECK(irq_bind(10, NULL) == IRQ_EINVAL);
    CHECK(irq_bind(10, &bad) == IRQ_EINVAL);
    CHECK(irq_unbind(-1) == IRQ_EINVAL);
    CHECK(irq_get_state(-1, &state) == IRQ_EINVAL);
    CHECK(irq_get_stats(IRQ_LINE_MAX, &stats) == IRQ_EINVAL);
    CHECK(strcmp(irq_state_name(IRQ_DISARMED), "DISARMED") == 0);
    CHECK(strcmp(irq_state_name(IRQ_ARMED), "ARMED") == 0);
    CHECK(strcmp(irq_state_name(IRQ_ACTIVE), "ACTIVE") == 0);
    CHECK(strcmp(irq_state_name((enum irq_state)99), "UNKNOWN") == 0);
}

int main(void)
{
    test_uninitialized();
    test_init_and_bind();
    test_spurious();
    test_one_shot_level();
    test_unbind_active();
    test_timer();
    test_multi_source_budget();
    test_invalid();

    if (g_failures == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }

    printf("%d TEST(S) FAILED\n", g_failures);
    return 1;
}
