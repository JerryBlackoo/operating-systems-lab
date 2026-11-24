#include "block.h"

static void finish(struct block_service *service, const struct block_request *request,
                   enum block_status status)
{
    request->buffer->borrowed = false;
    service->port->done(service->port->ctx, request, status,
                        status == BLOCK_OK ? request->bytes : 0);
}

enum block_status block_submit(struct block_service *service, uint32_t caller, unsigned dev,
                               enum block_op op, uint64_t lba, uint64_t blocks,
                               struct block_buffer *buffer, uint64_t *id)
{
    if (id)
        *id = 0;
    if (!service || !service->port || !service->port->submit || !service->port->done ||
        !caller || !id || dev >= BLOCK_DEVICES || !buffer || !buffer->data ||
        (op != BLOCK_READ && op != BLOCK_WRITE))
        return BLOCK_BAD_ARG;

    struct block_device *device = &service->devices[dev];
    if (!device->logical_bytes || lba >= device->capacity || !blocks ||
        blocks > device->capacity - lba || blocks > SIZE_MAX / device->logical_bytes)
        return BLOCK_BAD_ARG;

    size_t bytes = (size_t)blocks * device->logical_bytes;
    if (bytes > buffer->size ||
        (op == BLOCK_READ && !buffer->writable) || (op == BLOCK_WRITE && !buffer->readable))
        return BLOCK_BAD_ARG;

    if (device->len == BLOCK_PENDING || buffer->borrowed || service->last_id == UINT64_MAX)
        return BLOCK_BUSY;

    unsigned tail = (device->head + device->len) % BLOCK_PENDING;
    *id = ++service->last_id;
    buffer->borrowed = true;
    device->pending[tail] = (struct block_request){
        .id = *id, .caller = caller, .dev = dev, .op = op, .state = BLOCK_QUEUED,
        .lba = lba, .blocks = blocks, .bytes = bytes, .buffer = buffer,
    };
    device->len++;
    return BLOCK_OK;
}

enum block_status block_dispatch(struct block_service *service, unsigned dev)
{
    if (!service || !service->port || !service->port->submit || !service->port->done ||
        dev >= BLOCK_DEVICES || !service->devices[dev].logical_bytes)
        return BLOCK_BAD_ARG;

    struct block_device *device = &service->devices[dev];
    if (device->in_flight.id)
        return BLOCK_BUSY;
    if (!device->len)
        return BLOCK_EMPTY;

    struct block_request request = device->pending[device->head];
    enum block_status status = service->port->submit(service->port->ctx, &request);
    if (status == BLOCK_BUSY)
        return BLOCK_BUSY;

    device->pending[device->head] = (struct block_request){0};
    device->head = (device->head + 1) % BLOCK_PENDING;
    device->len--;

    if (status == BLOCK_OK) {
        request.state = BLOCK_IN_FLIGHT;
        device->in_flight = request;
        return BLOCK_OK;
    }

    request.state = BLOCK_DONE;
    finish(service, &request, status);
    return status;
}

bool block_complete(struct block_service *service, unsigned dev, uint64_t id,
                    enum block_status status)
{
    if (!service || !service->port || !service->port->done || dev >= BLOCK_DEVICES || !id ||
        (status != BLOCK_OK && status != BLOCK_IO_ERROR))
        return false;

    struct block_request *slot = &service->devices[dev].in_flight;
    if (slot->id != id)
        return false;

    struct block_request request = *slot;
    *slot = (struct block_request){0};
    request.state = BLOCK_DONE;
    finish(service, &request, status);
    return true;
}
