#include "nvme.h"

bool nvme_init(struct nvme *nvme, const struct nvme_port *port, struct nvme_namespace ns)
{
    if (!port || !port->sq || !port->cq || port->depth < 2 || !port->sq_id ||
        !port->sq_doorbell || !port->cq_doorbell || !ns.nsid || ns.nsid == UINT32_MAX ||
        !ns.lba_count || !ns.lba_bytes || ns.lba_bytes > NVME_PAGE ||
        (ns.lba_bytes & (ns.lba_bytes - 1)))
        return false;
    *nvme = (struct nvme){.port = port, .ns = ns, .cq_phase = 1};
    return true;
}

enum nvme_result nvme_submit(struct nvme *nvme, enum nvme_op op, uint64_t lba, uint32_t blocks,
                             struct nvme_dma *dma)
{
    if (nvme->state == NVME_FAULTED)
        return NVME_PROTOCOL_ERROR;
    if (nvme->state == NVME_IN_FLIGHT)
        return NVME_BUSY;
    if (!dma || !dma->address || (dma->address & (NVME_PAGE - 1)) ||
        !blocks || blocks > NVME_MAX_BLOCKS || lba >= nvme->ns.lba_count ||
        blocks > nvme->ns.lba_count - lba || blocks > NVME_PAGE / nvme->ns.lba_bytes ||
        (op == NVME_READ && !dma->device_write) || (op == NVME_WRITE && !dma->device_read) ||
        (op != NVME_READ && op != NVME_WRITE))
        return NVME_BAD_ARG;
    size_t bytes = (size_t)blocks * nvme->ns.lba_bytes;
    if (bytes > dma->capacity)
        return NVME_BAD_ARG;
    if (dma->borrowed)
        return NVME_BUSY;

    const struct nvme_port *port = nvme->port;
    uint16_t cid = nvme->next_cid++;
    dma->borrowed = true;
    nvme->in_flight = (struct nvme_request){.dma = dma, .bytes = bytes, .cid = cid};
    nvme->state = NVME_IN_FLIGHT;
    port->sq[nvme->sq_tail] = (struct nvme_sqe){
        .opcode = op, .cid = cid, .nsid = nvme->ns.nsid, .prp1 = dma->address,
        .cdw10 = (uint32_t)lba, .cdw11 = (uint32_t)(lba >> 32), .cdw12 = blocks - 1,
    };
    nvme->sq_tail = (nvme->sq_tail + 1) % port->depth;
    __atomic_thread_fence(__ATOMIC_RELEASE);
    port->sq_doorbell(port->ctx, nvme->sq_tail);
    return NVME_OK;
}

static void release(struct nvme *nvme)
{
    if (nvme->in_flight.dma)
        nvme->in_flight.dma->borrowed = false;
    nvme->in_flight = (struct nvme_request){0};
    nvme->state = NVME_IDLE;
}

enum nvme_result nvme_poll(struct nvme *nvme, struct nvme_done *done)
{
    if (!done)
        return NVME_BAD_ARG;
    if (nvme->state == NVME_FAULTED)
        return NVME_PROTOCOL_ERROR;
    const struct nvme_port *port = nvme->port;
    const volatile struct nvme_cqe *entry = &port->cq[nvme->cq_head];
    uint16_t status = entry->status;
    if ((status & 1) != nvme->cq_phase)
        return NVME_EMPTY;
    __atomic_thread_fence(__ATOMIC_ACQUIRE);
    if (nvme->state != NVME_IN_FLIGHT || entry->sq_id != port->sq_id ||
        entry->cid != nvme->in_flight.cid) {
        nvme->state = NVME_FAULTED;
        return NVME_PROTOCOL_ERROR;
    }

    *done = (struct nvme_done){
        .cid = nvme->in_flight.cid, .device_status = status >> 1,
        .bytes = (status >> 1) == 0 ? nvme->in_flight.bytes : 0,
    };
    nvme->cq_head = (nvme->cq_head + 1) % port->depth;
    if (!nvme->cq_head)
        nvme->cq_phase ^= 1;
    __atomic_thread_fence(__ATOMIC_RELEASE);
    port->cq_doorbell(port->ctx, nvme->cq_head);
    release(nvme);
    return NVME_OK;
}

enum nvme_result nvme_reset_done(struct nvme *nvme, bool controller_quiesced, struct nvme_done *done)
{
    if (!done || nvme->state != NVME_FAULTED)
        return NVME_BAD_ARG;
    if (!controller_quiesced)
        return NVME_BUSY;
    bool had_pending = nvme->in_flight.dma != NULL;
    if (had_pending)
        *done = (struct nvme_done){.cid = nvme->in_flight.cid, .protocol_error = true};
    nvme->sq_tail = nvme->cq_head = 0;
    nvme->cq_phase = 1;
    release(nvme);
    return had_pending ? NVME_OK : NVME_EMPTY;
}
