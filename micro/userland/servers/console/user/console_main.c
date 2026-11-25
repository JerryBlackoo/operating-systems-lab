/*
 * console_main.c — U-mode console 服务（M7）。
 *
 * shell 经 IPC 送 TAG_CONSOLE_WRITE / TAG_CONSOLE_READ / TAG_CONSOLE_MORE；
 * 载荷按 service_proto 的 24 字节分块（与 fs 服务一致），不经共享页。
 */
#include "abi/abi.h"
#include "runtime.h"
#include "service_proto.h"
#include "user_io.h"

enum { CONSOLE_LINE_CAP = 128 };

static char pending_line[CONSOLE_LINE_CAP];
static size_t pending_len;

static void pack24(const unsigned char *data, size_t n, uint64_t words[3])
{
    words[0] = 0;
    words[1] = 0;
    words[2] = 0;
    for (size_t i = 0; i < n && i < SVC_DATA_BYTES; i++) {
        words[i / 8U] |= (uint64_t)data[i] << (8U * (i % 8U));
    }
}

static void unpack24(uint64_t w1, uint64_t w2, uint64_t w3, unsigned char *out,
                     size_t n)
{
    uint64_t words[3] = {w1, w2, w3};

    for (size_t i = 0; i < n && i < SVC_DATA_BYTES; i++) {
        out[i] = (unsigned char)((words[i / 8U] >> (8U * (i % 8U))) & 0xffU);
    }
}

static void reply_msg(uint32_t reply, uint32_t tag, uint64_t w0, uint64_t w1,
                      uint64_t w2, uint64_t w3)
{
    struct abi_message msg = {
        .tag = tag,
        .words = {w0, w1, w2, w3},
    };

    (void)user_ipc_reply(reply, &msg);
}

static void register_to_sm(uint32_t sm_send, uint32_t sid)
{
    struct abi_message req = {
        .tag = TAG_SM_REGISTER,
        .words = {0, svc_name_pack("console"), sid, 0},
    };

    for (;;) {
        struct abi_message rep = {0};

        if (user_ipc_call(sm_send, &req, &rep) == ABI_OK &&
            rep.tag == TAG_SM_OK) {
            return;
        }
        abi_ecall(ABI_YIELD, (struct abi_regs){0});
    }
}

static void do_write(uint32_t reply, const struct abi_message *msg)
{
    size_t n = (size_t)msg->words[0];
    unsigned char buf[SVC_DATA_BYTES];

    if (n == 0 || n > SVC_DATA_BYTES) {
        reply_msg(reply, TAG_CONSOLE_ERROR, (uint64_t)ABI_BAD_ARG, 0, 0, 0);
        return;
    }
    unpack24(msg->words[1], msg->words[2], msg->words[3], buf, n);
    if (user_console_write(buf, n) != ABI_OK) {
        reply_msg(reply, TAG_CONSOLE_ERROR, (uint64_t)ABI_DENIED, 0, 0, 0);
        return;
    }
    reply_msg(reply, TAG_CONSOLE_WRITE_OK, n, 0, 0, 0);
}

static int fill_pending_line(void)
{
    size_t n = 0;

    pending_len = 0;
    for (;;) {
        int64_t st =
            user_console_read(pending_line, sizeof(pending_line) - 1U, &n);

        if (st == ABI_BUSY) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
            continue;
        }
        if (st != ABI_OK) {
            return (int)st;
        }
        break;
    }
    if (n >= sizeof(pending_line)) {
        n = sizeof(pending_line) - 1U;
    }
    pending_line[n] = '\0';
    pending_len = n;
    return ABI_OK;
}

static void send_chunk(uint32_t reply, uint32_t tag, size_t off, size_t *sent)
{
    size_t remain = pending_len > off ? pending_len - off : 0;
    size_t n = remain;

    if (n > SVC_DATA_BYTES) {
        n = SVC_DATA_BYTES;
    }
    {
        uint64_t words[3];

        pack24((const unsigned char *)pending_line + off, n, words);
        reply_msg(reply, tag, n, words[0], words[1], words[2]);
    }
    *sent = n;
}

static void do_read(uint32_t reply)
{
    int st = fill_pending_line();
    uint64_t words[3];
    size_t chunk;

    if (st != ABI_OK) {
        reply_msg(reply, TAG_CONSOLE_ERROR, (uint64_t)(int64_t)st, 0, 0, 0);
        return;
    }
    chunk = pending_len;
    if (chunk > SVC_DATA_BYTES) {
        chunk = SVC_DATA_BYTES;
    }
    pack24((const unsigned char *)pending_line, chunk, words);
    reply_msg(reply, TAG_CONSOLE_READ_OK, pending_len, words[0], words[1],
              words[2]);
}

static void do_more(uint32_t reply, size_t off)
{
    size_t chunk;

    if (off > pending_len) {
        reply_msg(reply, TAG_CONSOLE_ERROR, (uint64_t)ABI_BAD_ARG, 0, 0, 0);
        return;
    }
    send_chunk(reply, TAG_CONSOLE_MORE_OK, off, &chunk);
}

void console_start(uint32_t ready_send, uint64_t receipt, uint32_t console_recv,
                   uint32_t sid, uint32_t sm_send)
{
    struct abi_message ready = {
        .tag = TAG_READY,
        .words = {receipt, 0, 0, 0},
    };

    if (sm_send != 0) {
        register_to_sm(sm_send, sid);
        user_print("[console-svc] registered console");
    }

    if (user_ipc_send(ready_send, &ready) != ABI_OK) {
        for (;;) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
        }
    }
    user_print("[console-svc] serving");

    for (;;) {
        struct abi_message msg = {0};

        if (user_ipc_recv(console_recv, &msg) != ABI_OK || msg.reply == 0) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
            continue;
        }
        switch (msg.tag) {
        case TAG_CONSOLE_WRITE:
            do_write(msg.reply, &msg);
            break;
        case TAG_CONSOLE_READ:
            do_read(msg.reply);
            break;
        case TAG_CONSOLE_MORE:
            do_more(msg.reply, (size_t)msg.words[0]);
            break;
        default:
            reply_msg(msg.reply, TAG_CONSOLE_ERROR, (uint64_t)ABI_BAD_ARG, 0, 0,
                      0);
            break;
        }
    }
}