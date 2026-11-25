#include "nvme.h"
#include "demo.h"

enum { DEPTH = 2, SQ_ID = 3, LBA_BYTES = 512 };

static _Alignas(NVME_PAGE) uint8_t buffer[NVME_PAGE];
static struct controller_harness {
    struct nvme_sqe sq[DEPTH], last;
    struct nvme_cqe cq[DEPTH];
    unsigned sq_tail, cq_tail, cq_head;
    unsigned phase, sq_rings, cq_rings;
    bool has_command;
} controller = {.phase = 1};

static void ring_sq(void *ctx, uint16_t tail)
{
    struct controller_harness *c = ctx;
    CHECK(!c->has_command && tail == (c->sq_tail + 1) % DEPTH);
    c->last = c->sq[c->sq_tail];
    c->sq_tail = tail;
    c->sq_rings++;
    c->has_command = true;
}

static void ring_cq(void *ctx, uint16_t head)
{
    struct controller_harness *c = ctx;
    CHECK(head == (c->cq_head + 1) % DEPTH && head == c->cq_tail);
    c->cq_head = head;
    c->cq_rings++;
}

/* 模拟控制器复位：清空未消费的 CQ，队列指针和 phase 回到初始值。 */
static void rearm_controller(void)
{
    memset(controller.cq, 0, sizeof(controller.cq));
    controller.sq_tail = controller.cq_tail = controller.cq_head = 0;
    controller.phase = 1;
    controller.has_command = false;
}

static void post_completion(uint16_t status, bool wrong_cid, bool wrong_sq_id)
{
    struct controller_harness *c = &controller;
    CHECK(c->has_command && c->cq_head == c->cq_tail);
    void *data = (void *)(uintptr_t)c->last.prp1;
    size_t bytes = (c->last.cdw12 + 1) * LBA_BYTES;
    if (status == 0 && !wrong_cid && !wrong_sq_id) {
        if (c->last.opcode == NVME_READ)
            memset(data, 0xa1, bytes);
        else
            CHECK(((const uint8_t *)data)[0] == 0xa1);
    }
    c->cq[c->cq_tail] = (struct nvme_cqe){
        .sq_head = c->sq_tail, .sq_id = SQ_ID + (wrong_sq_id ? 1 : 0),
        .cid = c->last.cid + (wrong_cid ? 1 : 0), .status = (status << 1) | c->phase,
    };
    c->cq_tail = (c->cq_tail + 1) % DEPTH;
    if (!c->cq_tail)
        c->phase ^= 1;
    c->has_command = false;
}

/* 没有在途请求时由控制器注入的伪完成项。 */
static void post_unexpected(void)
{
    struct controller_harness *c = &controller;
    CHECK(!c->has_command && c->cq_head == c->cq_tail);
    c->cq[c->cq_tail] = (struct nvme_cqe){
        .sq_head = c->sq_tail, .sq_id = SQ_ID, .cid = UINT16_MAX, .status = c->phase,
    };
    c->cq_tail = (c->cq_tail + 1) % DEPTH;
    if (!c->cq_tail)
        c->phase ^= 1;
}

static void run(void)
{
    struct nvme nvme;
    const struct nvme_namespace ns = {1, 64, LBA_BYTES};
    const struct nvme_port port = {
        &controller, controller.sq, controller.cq, DEPTH, SQ_ID, ring_sq, ring_cq,
    };
    struct nvme_port broken = port;
    struct nvme_dma dma = {
        .address = (uintptr_t)buffer, .capacity = sizeof(buffer),
        .device_read = true, .device_write = true,
    };
    struct nvme_done done;

    CHECK(!nvme_init(&nvme, NULL, ns));
    broken.depth = 1;
    CHECK(!nvme_init(&nvme, &broken, ns));
    broken = port;
    broken.sq_id = 0;
    CHECK(!nvme_init(&nvme, &broken, ns));
    broken = port;
    broken.sq_doorbell = NULL;
    CHECK(!nvme_init(&nvme, &broken, ns));
    CHECK(!nvme_init(&nvme, &port, (struct nvme_namespace){0, 64, LBA_BYTES}));
    CHECK(!nvme_init(&nvme, &port, (struct nvme_namespace){1, 0, LBA_BYTES}));
    CHECK(!nvme_init(&nvme, &port, (struct nvme_namespace){1, 64, 0}));
    CHECK(!nvme_init(&nvme, &port, (struct nvme_namespace){1, 64, LBA_BYTES + 1}));
    CHECK(!nvme_init(&nvme, &port, (struct nvme_namespace){1, 64, 2 * NVME_PAGE}));
    CHECK(nvme_init(&nvme, &port, ns));

    CHECK(nvme_poll(&nvme, NULL) == NVME_BAD_ARG);
    CHECK(nvme_poll(&nvme, &done) == NVME_EMPTY);
    CHECK(nvme_reset_done(&nvme, true, NULL) == NVME_BAD_ARG);

    struct nvme_dma misaligned = dma, tight = dma, read_only = dma, write_only = dma;
    misaligned.address = dma.address + 1;
    tight.capacity = LBA_BYTES - 1;
    read_only.device_write = false;
    write_only.device_read = false;
    CHECK(nvme_submit(&nvme, NVME_READ, 0, 1, NULL) == NVME_BAD_ARG);
    CHECK(nvme_submit(&nvme, NVME_READ, 0, 0, &dma) == NVME_BAD_ARG);
    CHECK(nvme_submit(&nvme, NVME_READ, 0, 1, &misaligned) == NVME_BAD_ARG);
    CHECK(nvme_submit(&nvme, NVME_READ, 0, 1, &tight) == NVME_BAD_ARG);
    CHECK(nvme_submit(&nvme, NVME_READ, 0, 1, &read_only) == NVME_BAD_ARG);
    CHECK(nvme_submit(&nvme, NVME_WRITE, 0, 1, &write_only) == NVME_BAD_ARG);
    CHECK(nvme_submit(&nvme, NVME_READ, 63, 2, &dma) == NVME_BAD_ARG);
    CHECK(nvme_submit(&nvme, NVME_READ, 64, 1, &dma) == NVME_BAD_ARG);
    CHECK(nvme_submit(&nvme, NVME_READ, 0, NVME_PAGE / LBA_BYTES + 1, &dma) == NVME_BAD_ARG);
    CHECK(nvme_submit(&nvme, (enum nvme_op)7, 0, 1, &dma) == NVME_BAD_ARG);
    CHECK(controller.sq_rings == 0 && !dma.borrowed);

    for (unsigned i = 0; i < 3; i++) {
        enum nvme_op op = i == 0 ? NVME_READ : NVME_WRITE;
        uint16_t status = i == 2 ? 1 : 0;
        CHECK(nvme_submit(&nvme, op, 3, 2, &dma) == NVME_OK && dma.borrowed);
        CHECK(controller.last.opcode == op && controller.last.nsid == 1);
        CHECK(controller.last.cdw10 == 3 && controller.last.cdw11 == 0 && controller.last.cdw12 == 1);
        CHECK(controller.last.prp1 == dma.address && controller.last.prp2 == 0);
        CHECK(nvme_submit(&nvme, op, 3, 2, &dma) == NVME_BUSY);
        CHECK(nvme_poll(&nvme, &done) == NVME_EMPTY);
        post_completion(status, false, false);
        CHECK(nvme_poll(&nvme, &done) == NVME_OK && done.device_status == status);
        CHECK(done.cid == controller.last.cid && done.bytes == (status ? 0 : 2 * LBA_BYTES));
        CHECK(!dma.borrowed && buffer[2 * LBA_BYTES - 1] == 0xa1);
        CHECK(nvme_poll(&nvme, &done) == NVME_EMPTY);
        CHECK(nvme.cq_phase == (i < DEPTH - 1 ? 1u : 0u));
    }

    CHECK(nvme_submit(&nvme, NVME_READ, 0, NVME_PAGE / LBA_BYTES, &dma) == NVME_OK);
    CHECK(controller.last.cdw12 == NVME_PAGE / LBA_BYTES - 1);
    post_completion(0, false, false);
    CHECK(nvme_poll(&nvme, &done) == NVME_OK && done.bytes == NVME_PAGE);
    CHECK(controller.sq_rings == 4 && controller.cq_rings == 4);

    CHECK(nvme_submit(&nvme, NVME_READ, 3, 2, &dma) == NVME_OK);
    post_completion(0, true, false);
    CHECK(nvme_poll(&nvme, &done) == NVME_PROTOCOL_ERROR && dma.borrowed);
    CHECK(nvme_submit(&nvme, NVME_READ, 3, 2, &dma) == NVME_PROTOCOL_ERROR);
    CHECK(nvme_reset_done(&nvme, false, &done) == NVME_BUSY && dma.borrowed);
    rearm_controller();
    CHECK(nvme_reset_done(&nvme, true, &done) == NVME_OK && done.protocol_error);
    CHECK(!dma.borrowed && nvme.state == NVME_IDLE);
    CHECK(nvme_submit(&nvme, NVME_READ, 3, 2, &dma) == NVME_OK);
    post_completion(0, false, false);
    CHECK(nvme_poll(&nvme, &done) == NVME_OK && !dma.borrowed);

    CHECK(nvme_submit(&nvme, NVME_WRITE, 4, 1, &dma) == NVME_OK);
    post_completion(0, false, true);
    CHECK(nvme_poll(&nvme, &done) == NVME_PROTOCOL_ERROR && dma.borrowed);
    CHECK(nvme_reset_done(&nvme, false, &done) == NVME_BUSY);
    rearm_controller();
    CHECK(nvme_reset_done(&nvme, true, &done) == NVME_OK && done.protocol_error);
    CHECK(!dma.borrowed && nvme.state == NVME_IDLE);

    post_unexpected();
    CHECK(nvme_poll(&nvme, &done) == NVME_PROTOCOL_ERROR && nvme.state == NVME_FAULTED);
    CHECK(nvme_reset_done(&nvme, true, &done) == NVME_EMPTY);
    rearm_controller();
    CHECK(nvme_submit(&nvme, NVME_READ, 3, 2, &dma) == NVME_OK);
    post_completion(0, false, false);
    CHECK(nvme_poll(&nvme, &done) == NVME_OK && !dma.borrowed);
    CHECK(controller.sq_rings == 8 && controller.cq_rings == 6);
    demo_log("NVMe: validation, SQE encoding, CQ phase wrap, DMA lifetime and fault recovery");
}

DEMO_MODULE(nvme, true, run, NULL);
