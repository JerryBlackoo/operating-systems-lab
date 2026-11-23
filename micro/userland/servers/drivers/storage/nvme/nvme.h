#ifndef MICRO_MODEL_NVME_H
#define MICRO_MODEL_NVME_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum { NVME_PAGE = 4096 };
enum nvme_op { NVME_WRITE = 1, NVME_READ = 2 };
enum nvme_state { NVME_IDLE, NVME_IN_FLIGHT, NVME_FAULTED };
enum nvme_result { NVME_OK, NVME_BAD_ARG, NVME_BUSY, NVME_EMPTY, NVME_PROTOCOL_ERROR };

struct nvme_sqe {
    uint8_t opcode, flags;
    uint16_t cid;
    uint32_t nsid;
    uint64_t reserved, metadata, prp1, prp2;
    uint32_t cdw10, cdw11, cdw12, cdw13, cdw14, cdw15;
};

struct nvme_cqe {
    uint32_t result, reserved;
    uint16_t sq_head, sq_id, cid, status;
};

_Static_assert(sizeof(struct nvme_sqe) == 64 && offsetof(struct nvme_sqe, prp1) == 24, "NVMe SQE");
_Static_assert(sizeof(struct nvme_cqe) == 16 && offsetof(struct nvme_cqe, status) == 14, "NVMe CQE");

struct nvme_dma {
    uintptr_t address;
    size_t capacity;
    bool device_read, device_write, borrowed;
};
struct nvme_namespace { uint32_t id; uint64_t blocks; size_t lba_bytes; };
struct nvme_port {
    void *ctx;
    volatile struct nvme_sqe *sq;
    const volatile struct nvme_cqe *cq;
    uint16_t depth, sq_id;
    void (*sq_doorbell)(void *, uint16_t tail);
    void (*cq_doorbell)(void *, uint16_t head);
};
struct nvme_done { uint16_t cid, device_status; size_t bytes; bool protocol_error; };

struct nvme {
    const struct nvme_port *port;
    struct nvme_namespace ns;
    enum nvme_state state;
    uint16_t sq_tail, cq_head, next_cid;
    unsigned cq_phase;
    struct {
        struct nvme_dma *dma;
        size_t bytes;
        uint16_t cid;
    } pending;
};

bool nvme_init(struct nvme *, const struct nvme_port *, struct nvme_namespace);
enum nvme_result nvme_submit(struct nvme *, enum nvme_op, uint64_t lba, uint32_t blocks,
                             struct nvme_dma *);
enum nvme_result nvme_poll(struct nvme *, struct nvme_done *);
enum nvme_result nvme_reset_done(struct nvme *, bool controller_quiesced, struct nvme_done *);

#endif
