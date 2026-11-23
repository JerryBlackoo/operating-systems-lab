#include "block.h"
#include "demo.h"

enum { LOGICAL_BYTES = 512, DISK_BLOCKS = 8 };

static struct disk_fixture {
    uint8_t disk[DISK_BLOCKS * LOGICAL_BYTES];
    struct block_request pending;
    enum block_status dispatch_status;
    unsigned completed;
    uint64_t last_id;
    enum block_status status;
    size_t bytes;
} disk;

static enum block_status submit(void *ctx, const struct block_request *request)
{
    struct disk_fixture *d = ctx;
    CHECK(request->buffer->borrowed && !d->pending.id);
    if (d->dispatch_status != BLOCK_OK)
        return d->dispatch_status;
    d->pending = *request;
    return BLOCK_OK;
}

static void done(void *ctx, const struct block_request *request, enum block_status status,
                 size_t bytes)
{
    struct disk_fixture *d = ctx;
    CHECK(!request->buffer->borrowed && request->caller == 10);
    d->completed++;
    d->last_id = request->id;
    d->status = status;
    d->bytes = bytes;
}

static void finish_io(struct block_service *service, enum block_status status)
{
    struct block_request request = disk.pending;
    CHECK(request.id && request.buffer->borrowed);
    uint8_t *address = &disk.disk[request.lba * LOGICAL_BYTES];
    if (status == BLOCK_OK && request.op == BLOCK_READ)
        memcpy(request.buffer->data, address, request.bytes);
    if (status == BLOCK_OK && request.op == BLOCK_WRITE)
        memcpy(address, request.buffer->data, request.bytes);
    disk.pending = (struct block_request){0};
    CHECK(block_complete(service, request.dev, request.id, status));
}

static void run(void)
{
    const struct block_port port = {&disk, submit, done};
    struct block_service service = {
        .port = &port, .devices = {{.capacity = DISK_BLOCKS, .logical_bytes = LOGICAL_BYTES}},
    };
    uint8_t sample[LOGICAL_BYTES], result[LOGICAL_BYTES];
    memset(sample, 0xc7, sizeof(sample));
    struct block_buffer input = {.data = sample, .size = sizeof(sample), .readable = true};
    struct block_buffer output = {.data = result, .size = sizeof(result), .writable = true};
    uint64_t writer, reader, failure;

    CHECK(block_submit(&service, 10, 0, BLOCK_READ, 7, 2, &output, &reader) == BLOCK_BAD_ARG);
    CHECK(!output.borrowed && service.last_id == 0);
    CHECK(block_submit(&service, 10, 0, BLOCK_READ, 2, 1, &input, &reader) == BLOCK_BAD_ARG);
    CHECK(block_submit(&service, 10, 0, BLOCK_WRITE, 2, 1, &input, &writer) == BLOCK_OK);
    CHECK(block_submit(&service, 10, 0, BLOCK_READ, 2, 1, &output, &reader) == BLOCK_OK);
    disk.dispatch_status = BLOCK_BUSY;
    CHECK(block_dispatch(&service, 0) == BLOCK_BUSY && service.devices[0].len == 2);
    disk.dispatch_status = BLOCK_OK;
    CHECK(block_dispatch(&service, 0) == BLOCK_OK && disk.pending.id == writer);
    CHECK(block_dispatch(&service, 0) == BLOCK_BUSY);
    CHECK(!block_complete(&service, 1, writer, BLOCK_OK));
    finish_io(&service, BLOCK_OK);
    CHECK(disk.completed == 1 && disk.last_id == writer && disk.bytes == LOGICAL_BYTES);
    CHECK(block_dispatch(&service, 0) == BLOCK_OK && disk.pending.id == reader);
    CHECK(!block_complete(&service, 0, writer, BLOCK_OK));
    finish_io(&service, BLOCK_OK);
    CHECK(disk.completed == 2 && disk.last_id == reader && !memcmp(sample, result, LOGICAL_BYTES));
    CHECK(block_submit(&service, 10, 0, BLOCK_READ, 2, 1, &output, &failure) == BLOCK_OK);
    CHECK(block_dispatch(&service, 0) == BLOCK_OK);
    finish_io(&service, BLOCK_IO_ERROR);
    CHECK(disk.completed == 3 && disk.last_id == failure && disk.bytes == 0);
    CHECK(disk.status == BLOCK_IO_ERROR && !input.borrowed && !output.borrowed);
    demo_log("block: FIFO, busy retry, exact completion and buffer return");
}

DEMO_MODULE(block, true, run, NULL);
