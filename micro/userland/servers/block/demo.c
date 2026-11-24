#include "block.h"
#include "demo.h"

enum { CALLER = 10, SECTOR = 512, SECTORS = 8, PAGE = 4096, PAGES = 2 };

struct fixture {
    uint8_t data[BLOCK_DEVICES][PAGES * PAGE];
    struct block_request command[BLOCK_DEVICES];
    enum block_status accept[BLOCK_DEVICES];
    unsigned submitted[BLOCK_DEVICES], completed[BLOCK_DEVICES];
    uint64_t last_id[BLOCK_DEVICES];
    enum block_status last_status[BLOCK_DEVICES];
    enum block_state last_state[BLOCK_DEVICES];
    size_t last_bytes[BLOCK_DEVICES];
};

static struct fixture disk;
static uint8_t source[PAGE], target[PAGE];

static enum block_status submit(void *ctx, const struct block_request *request)
{
    struct fixture *f = ctx;
    unsigned dev = request->dev;

    CHECK(dev < BLOCK_DEVICES);
    CHECK(request->state == BLOCK_QUEUED && request->buffer->borrowed);
    CHECK(!f->command[dev].id);
    if (f->accept[dev] != BLOCK_OK)
        return f->accept[dev];
    f->command[dev] = *request;
    f->submitted[dev]++;
    return BLOCK_OK;
}

static void done(void *ctx, const struct block_request *request, enum block_status status,
                 size_t bytes)
{
    struct fixture *f = ctx;
    unsigned dev = request->dev;

    CHECK(dev < BLOCK_DEVICES);
    CHECK(request->state == BLOCK_DONE && !request->buffer->borrowed);
    CHECK(request->caller == CALLER);
    f->completed[dev]++;
    f->last_id[dev] = request->id;
    f->last_status[dev] = status;
    f->last_state[dev] = request->state;
    f->last_bytes[dev] = bytes;
}

static void finish_io(struct block_service *service, unsigned dev, enum block_status status)
{
    struct block_request request = disk.command[dev];
    struct block_device *device = &service->devices[dev];

    CHECK(request.id && request.dev == dev);
    CHECK(device->in_flight.id == request.id && device->in_flight.state == BLOCK_IN_FLIGHT);

    uint8_t *address = disk.data[dev] + request.lba * device->logical_bytes;
    if (status == BLOCK_OK && request.op == BLOCK_READ)
        memcpy(request.buffer->data, address, request.bytes);
    if (status == BLOCK_OK && request.op == BLOCK_WRITE)
        memcpy(address, request.buffer->data, request.bytes);

    disk.command[dev] = (struct block_request){0};
    CHECK(block_complete(service, dev, request.id, status));
    CHECK(!device->in_flight.id && !request.buffer->borrowed);
}

static uint64_t expect_submit(struct block_service *service, unsigned dev, enum block_op op,
                              uint64_t lba, uint64_t blocks, struct block_buffer *buffer)
{
    uint64_t id = 0;
    CHECK(block_submit(service, CALLER, dev, op, lba, blocks, buffer, &id) == BLOCK_OK);
    CHECK(id && id == service->last_id && buffer->borrowed);
    return id;
}

static void expect_busy(struct block_service *service, unsigned dev, enum block_op op,
                        uint64_t lba, uint64_t blocks, struct block_buffer *buffer)
{
    uint64_t before = service->last_id, id = 0;
    CHECK(block_submit(service, CALLER, dev, op, lba, blocks, buffer, &id) == BLOCK_BUSY);
    CHECK(!id && service->last_id == before && !buffer->borrowed);
}

static void run(void)
{
    const struct block_port port = {&disk, submit, done};
    struct block_service service = {
        .port = &port,
        .devices = {
            {.capacity = SECTORS, .logical_bytes = SECTOR},
            {.capacity = PAGES, .logical_bytes = PAGE},
        },
    };
    uint8_t slots[BLOCK_PENDING][SECTOR];
    struct block_buffer queued[BLOCK_PENDING];
    uint64_t ids[BLOCK_PENDING], id;

    memset(&disk, 0, sizeof(disk));
    struct block_buffer reader = {.data = target, .size = sizeof(target), .writable = true};
    struct block_buffer writer = {.data = source, .size = sizeof(source), .readable = true};
    struct block_buffer both = {.data = target, .size = sizeof(target), .readable = true,
                                .writable = true};
    struct block_buffer tiny = {.data = target, .size = SECTOR - 1, .writable = true};
    struct block_buffer blank = {.size = PAGE, .writable = true};

    /* 校验：调用者、设备、范围、长度、权限、缓冲区容量与操作码。 */
    uint64_t bad = 9;
    CHECK(block_submit(&service, 0, 0, BLOCK_READ, 0, 1, &reader, &bad) == BLOCK_BAD_ARG);
    CHECK(!bad && !reader.borrowed);
    CHECK(block_submit(&service, CALLER, BLOCK_DEVICES, BLOCK_READ, 0, 1, &reader, &bad) ==
          BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, BLOCK_READ, SECTORS, 1, &reader, &bad) == BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, BLOCK_READ, SECTORS - 1, 2, &reader, &bad) ==
          BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, BLOCK_READ, 0, 0, &reader, &bad) == BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, BLOCK_READ, 0, 1, &tiny, &bad) == BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, BLOCK_READ, 0, 1, &writer, &bad) == BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, BLOCK_WRITE, 0, 1, &reader, &bad) == BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, (enum block_op)2, 0, 1, &both, &bad) == BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, BLOCK_READ, 0, 1, &blank, &bad) == BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, BLOCK_READ, 0, 1, NULL, &bad) == BLOCK_BAD_ARG);
    CHECK(block_submit(&service, CALLER, 0, BLOCK_READ, 0, 1, &reader, NULL) == BLOCK_BAD_ARG);
    CHECK(service.last_id == 0);

    /* 空队列与未初始化设备。 */
    CHECK(block_dispatch(&service, BLOCK_DEVICES) == BLOCK_BAD_ARG);
    CHECK(block_dispatch(&service, 0) == BLOCK_EMPTY);
    CHECK(block_dispatch(&service, 1) == BLOCK_EMPTY);
    CHECK(!block_complete(&service, 0, 1, BLOCK_OK));

    /* 有界 FIFO：第四个之后拒绝入队，且不消耗请求 ID。 */
    for (unsigned i = 0; i < BLOCK_PENDING; i++)
        queued[i] = (struct block_buffer){
            .data = slots[i], .size = SECTOR, .readable = true, .writable = true,
        };
    memset(slots, 0x7e, sizeof(slots));
    ids[0] = expect_submit(&service, 0, BLOCK_WRITE, 0, 1, &queued[0]);
    CHECK(service.devices[0].pending[service.devices[0].head].state == BLOCK_QUEUED);
    CHECK(service.devices[0].len == 1);

    uint64_t before = service.last_id, same = 0;
    CHECK(block_submit(&service, CALLER, 0, BLOCK_WRITE, 0, 1, &queued[0], &same) == BLOCK_BUSY);
    CHECK(!same && service.last_id == before && queued[0].borrowed);

    for (unsigned i = 1; i < BLOCK_PENDING; i++) {
        ids[i] = expect_submit(&service, 0, BLOCK_WRITE, i, 1, &queued[i]);
        CHECK(ids[i] == ids[i - 1] + 1);
        CHECK(service.devices[0].len == i + 1);
    }
    expect_busy(&service, 0, BLOCK_WRITE, 0, 1, &both);
    CHECK(service.last_id == ids[BLOCK_PENDING - 1]);

    /* 单在途槽：派发队首后拒绝再次派发，完成必须匹配设备与 ID。 */
    CHECK(block_dispatch(&service, 0) == BLOCK_OK);
    CHECK(service.devices[0].len == BLOCK_PENDING - 1);
    CHECK(service.devices[0].in_flight.id == ids[0]);
    CHECK(service.devices[0].in_flight.state == BLOCK_IN_FLIGHT);
    CHECK(block_dispatch(&service, 0) == BLOCK_BUSY);
    CHECK(block_dispatch(&service, 1) == BLOCK_EMPTY);
    CHECK(!block_complete(&service, 1, ids[0], BLOCK_OK));
    CHECK(!block_complete(&service, 0, ids[1], BLOCK_OK));
    CHECK(!block_complete(&service, 0, ids[0], BLOCK_BUSY));
    finish_io(&service, 0, BLOCK_OK);
    CHECK(disk.completed[0] == 1 && disk.last_id[0] == ids[0]);
    CHECK(disk.last_status[0] == BLOCK_OK && disk.last_state[0] == BLOCK_DONE);
    CHECK(disk.last_bytes[0] == SECTOR && !queued[0].borrowed);
    CHECK(!block_complete(&service, 0, ids[0], BLOCK_OK));

    for (unsigned i = 1; i < BLOCK_PENDING; i++) {
        CHECK(block_dispatch(&service, 0) == BLOCK_OK);
        CHECK(service.devices[0].in_flight.id == ids[i]);
        finish_io(&service, 0, i == 1 ? BLOCK_IO_ERROR : BLOCK_OK);
        CHECK(disk.last_id[0] == ids[i]);
        if (i == 1)
            CHECK(disk.last_status[0] == BLOCK_IO_ERROR && disk.last_bytes[0] == 0);
    }
    CHECK(service.devices[0].len == 0 && !service.devices[0].in_flight.id);
    CHECK(block_dispatch(&service, 0) == BLOCK_EMPTY);
    for (unsigned i = 0; i < BLOCK_PENDING; i++)
        CHECK(!queued[i].borrowed);

    /* 读写数据路径与失败完成：失败不复制数据，也不返回字节数。 */
    memset(source, 0xa5, sizeof(source));
    id = expect_submit(&service, 0, BLOCK_WRITE, 2, 2, &writer);
    CHECK(block_dispatch(&service, 0) == BLOCK_OK);
    CHECK(disk.command[0].id == id && disk.command[0].op == BLOCK_WRITE);
    CHECK(disk.command[0].lba == 2 && disk.command[0].blocks == 2);
    CHECK(disk.command[0].bytes == 2 * SECTOR && disk.command[0].caller == CALLER);
    finish_io(&service, 0, BLOCK_OK);
    CHECK(!memcmp(disk.data[0] + 2 * SECTOR, source, 2 * SECTOR));

    memset(target, 0, sizeof(target));
    expect_submit(&service, 0, BLOCK_READ, 2, 2, &reader);
    CHECK(block_dispatch(&service, 0) == BLOCK_OK);
    finish_io(&service, 0, BLOCK_OK);
    CHECK(!memcmp(target, source, 2 * SECTOR));
    CHECK(disk.last_status[0] == BLOCK_OK && disk.last_bytes[0] == 2 * SECTOR);

    memset(target, 0x11, sizeof(target));
    expect_submit(&service, 0, BLOCK_READ, 2, 2, &reader);
    CHECK(block_dispatch(&service, 0) == BLOCK_OK);
    finish_io(&service, 0, BLOCK_IO_ERROR);
    CHECK(disk.last_status[0] == BLOCK_IO_ERROR && disk.last_bytes[0] == 0);
    CHECK(target[0] == 0x11 && target[2 * SECTOR - 1] == 0x11);

    /* 驱动暂忙：队首保留、缓冲区继续借用，下一次派发原样提交。 */
    struct block_buffer busy = {.data = slots[0], .size = SECTOR, .readable = true,
                                .writable = true};
    unsigned submitted = disk.submitted[0];
    disk.accept[0] = BLOCK_BUSY;
    id = expect_submit(&service, 0, BLOCK_WRITE, 3, 1, &busy);
    CHECK(block_dispatch(&service, 0) == BLOCK_BUSY);
    CHECK(service.devices[0].len == 1 && !service.devices[0].in_flight.id);
    CHECK(disk.submitted[0] == submitted && busy.borrowed);
    disk.accept[0] = BLOCK_OK;
    CHECK(block_dispatch(&service, 0) == BLOCK_OK);
    CHECK(service.devices[0].len == 0 && service.devices[0].in_flight.id == id);
    finish_io(&service, 0, BLOCK_OK);
    CHECK(!busy.borrowed);

    /* 驱动永久拒绝：直接以错误结束，不占在途槽，队首已移除。 */
    struct block_buffer rejected = {.data = slots[1], .size = SECTOR, .readable = true,
                                    .writable = true};
    unsigned completed = disk.completed[0];
    disk.accept[0] = BLOCK_IO_ERROR;
    id = expect_submit(&service, 0, BLOCK_WRITE, 4, 1, &rejected);
    CHECK(block_dispatch(&service, 0) == BLOCK_IO_ERROR);
    CHECK(service.devices[0].len == 0 && !service.devices[0].in_flight.id);
    CHECK(disk.completed[0] == completed + 1 && disk.last_id[0] == id);
    CHECK(disk.last_status[0] == BLOCK_IO_ERROR && disk.last_state[0] == BLOCK_DONE);
    CHECK(disk.last_bytes[0] == 0 && !rejected.borrowed && !disk.command[0].id);
    CHECK(block_dispatch(&service, 0) == BLOCK_EMPTY);
    disk.accept[0] = BLOCK_OK;

    /* 双设备：不同逻辑块大小、各自队列与在途槽，完成可以乱序。 */
    struct block_buffer page_writer = {.data = source, .size = PAGE, .readable = true};
    struct block_buffer page_io = {.data = target, .size = PAGE, .readable = true,
                                   .writable = true};
    memset(source, 0x3c, PAGE);
    uint64_t page_id = expect_submit(&service, 1, BLOCK_WRITE, 1, 1, &page_writer);
    uint64_t sector_id = expect_submit(&service, 0, BLOCK_WRITE, 5, 1, &page_io);
    CHECK(sector_id == page_id + 1);
    CHECK(block_dispatch(&service, 1) == BLOCK_OK);
    CHECK(block_dispatch(&service, 0) == BLOCK_OK);
    CHECK(service.devices[1].in_flight.id == page_id);
    CHECK(service.devices[0].in_flight.id == sector_id);
    CHECK(block_dispatch(&service, 0) == BLOCK_BUSY);
    CHECK(block_dispatch(&service, 1) == BLOCK_BUSY);
    finish_io(&service, 1, BLOCK_OK);
    CHECK(service.devices[0].in_flight.id == sector_id && !service.devices[1].in_flight.id);
    CHECK(disk.last_id[1] == page_id && disk.last_bytes[1] == PAGE);
    CHECK(!memcmp(disk.data[1] + PAGE, source, PAGE));
    finish_io(&service, 0, BLOCK_OK);
    CHECK(disk.last_id[0] == sector_id && !page_writer.borrowed && !page_io.borrowed);

    /* 环形队列回绕：连续提交、派发、完成六次。 */
    struct block_buffer cyclic = {.data = target, .size = SECTOR, .readable = true,
                                  .writable = true};
    for (unsigned i = 0; i < 6; i++) {
        id = expect_submit(&service, 0, BLOCK_READ, i % SECTORS, 1, &cyclic);
        CHECK(service.devices[0].len == 1);
        CHECK(block_dispatch(&service, 0) == BLOCK_OK);
        CHECK(service.devices[0].len == 0 && service.devices[0].in_flight.id == id);
        finish_io(&service, 0, BLOCK_OK);
        CHECK(!cyclic.borrowed);
    }

    CHECK(disk.submitted[0] && disk.completed[0]);
    demo_log("block: FIFO, single in-flight slot, busy retry, permanent rejection and delivery");
}

DEMO_MODULE(block, true, run, NULL);
