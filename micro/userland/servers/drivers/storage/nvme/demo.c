#include "nvme.h"
#include "demo.h"

enum { DEPTH = 2, SQ_ID = 3, LBA_BYTES = 512 };

static _Alignas(NVME_PAGE) uint8_t buffer[NVME_PAGE];
static struct controller_fixture {
    struct nvme_sqe sq[DEPTH], last;
    struct nvme_cqe cq[DEPTH];
    unsigned sq_tail, cq_tail, cq_head;
    unsigned phase, sq_rings, cq_rings;
    bool has_command;
} controller = {.phase = 1};

static void ring_sq(void *ctx, uint16_t tail)
{
    struct controller_fixture *c = ctx;
    CHECK(!c->has_command && tail == (c->sq_tail + 1) % DEPTH);
    c->last = c->sq[c->sq_tail];
    c->sq_tail = tail;
    c->sq_rings++;
    c->has_command = true;
}

static void ring_cq(void *ctx, uint16_t head)
{
    struct controller_fixture *c = ctx;
    CHECK(head == (c->cq_head + 1) % DEPTH && head == c->cq_tail);
    c->cq_head = head;
    c->cq_rings++;
}

static void post_completion(uint16_t status, bool wrong_cid)
{
    struct controller_fixture *c = &controller;
    CHECK(c->has_command && c->cq_head == c->cq_tail);
    void *data = (void *)(uintptr_t)c->last.prp1;
    size_t bytes = (c->last.cdw12 + 1) * LBA_BYTES;
    if (status == 0 && !wrong_cid) {
        if (c->last.opcode == NVME_READ)
            memset(data, 0xa1, bytes);
        else
            CHECK(((const uint8_t *)data)[0] == 0xa1);
    }
    c->cq[c->cq_tail] = (struct nvme_cqe){
        .sq_head = c->sq_tail, .sq_id = SQ_ID,
        .cid = c->last.cid + (wrong_cid ? 1 : 0), .status = (status << 1) | c->phase,
    };
    c->cq_tail = (c->cq_tail + 1) % DEPTH;
    if (!c->cq_tail)
        c->phase ^= 1;
    c->has_command = false;
}

static void run(void)
{
    struct nvme nvme;
    const struct nvme_port port = {
        &controller, controller.sq, controller.cq, DEPTH, SQ_ID, ring_sq, ring_cq,
    };
    struct nvme_dma dma = {
        .address = (uintptr_t)buffer, .capacity = sizeof(buffer),
        .device_read = true, .device_write = true,
    };
    struct nvme_done done;
    CHECK(nvme_init(&nvme, &port, (struct nvme_namespace){1, 64, LBA_BYTES}));
    CHECK(nvme_poll(&nvme, &done) == NVME_EMPTY);
    CHECK(nvme_submit(&nvme, NVME_READ, 63, 2, &dma) == NVME_BAD_ARG);
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
        post_completion(status, false);
        CHECK(nvme_poll(&nvme, &done) == NVME_OK && done.device_status == status);
        CHECK(done.cid == controller.last.cid && done.bytes == (status ? 0 : 2 * LBA_BYTES));
        CHECK(!dma.borrowed && buffer[2 * LBA_BYTES - 1] == 0xa1);
        CHECK(nvme_poll(&nvme, &done) == NVME_EMPTY);
        CHECK(nvme.cq_phase == (i < DEPTH - 1 ? 1u : 0u));
    }
    CHECK(controller.sq_rings == 3 && controller.cq_rings == 3);
    CHECK(nvme_submit(&nvme, NVME_READ, 3, 2, &dma) == NVME_OK);
    post_completion(0, true);
    CHECK(nvme_poll(&nvme, &done) == NVME_PROTOCOL_ERROR && dma.borrowed);
    CHECK(nvme_reset_done(&nvme, false, &done) == NVME_BUSY && dma.borrowed);
    memset(controller.cq, 0, sizeof(controller.cq));
    controller.cq_head = controller.cq_tail = controller.sq_tail = 0;
    controller.phase = 1;
    CHECK(nvme_reset_done(&nvme, true, &done) == NVME_OK && done.protocol_error);
    CHECK(!dma.borrowed && nvme.state == NVME_IDLE && controller.cq_rings == 3);
    CHECK(nvme_submit(&nvme, NVME_READ, 3, 2, &dma) == NVME_OK);
    post_completion(0, false);
    CHECK(nvme_poll(&nvme, &done) == NVME_OK && !dma.borrowed);
    demo_log("NVMe: SQE encoding, CQ wrap/phase, DMA lifetime and fault recovery");
}

DEMO_MODULE(nvme, true, run, NULL);
