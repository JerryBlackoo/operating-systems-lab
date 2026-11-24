#include "abi.h"
#include "demo.h"
#include "csr.h"
#include "trap.h"

enum {
    ABI_DEMO_ENDPOINT = 0x10002u,
    ABI_DEMO_TAG = 7,
    ABI_DEMO_REPLY = 9,
    ABI_DEMO_WORD = 11,
};

/*
 * 内核侧替身：只用 abi.h 的共享编解码处理输入/输出向量。
 * 它不实现真实 IPC，只核对分发层看到的寄存器形状。
 */
static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    if (frame->scause != EXC_ECALL_U || (frame->sstatus & SSTATUS_SPP))
        return NULL;

    const struct abi_regs in = {{
        frame->a0, frame->a1, frame->a2, frame->a3, frame->a4, frame->a5,
    }};
    struct abi_regs out;

    switch (frame->a7) {
    case ABI_IPC_SEND: {
        uint32_t endpoint = 0;
        struct abi_message sent = abi_decode_message(in);

        CHECK(abi_arg_u32(in, 0, &endpoint) && endpoint == ABI_DEMO_ENDPOINT);
        CHECK(sent.tag == ABI_DEMO_TAG && sent.reply == 0);
        CHECK(sent.words[0] == ABI_DEMO_WORD);
        CHECK(sent.words[1] == 0 && sent.words[2] == 0 && sent.words[3] == 0);
        out = abi_result(ABI_OK, NULL);
        break;
    }
    case ABI_IPC_RECV: {
        uint32_t endpoint = 0;
        const struct abi_message response = {
            .tag = ABI_DEMO_TAG,
            .reply = ABI_DEMO_REPLY,
            .words = {ABI_DEMO_WORD},
        };

        CHECK(abi_arg_u32(in, 0, &endpoint) && endpoint == ABI_DEMO_ENDPOINT);
        CHECK(abi_tail_zero(in, 1));
        out = abi_result(ABI_OK, &response);
        break;
    }
    default:
        out = abi_result(ABI_BAD_NR, NULL);
        break;
    }

    frame->a0 = out.a[0];
    frame->a1 = out.a[1];
    frame->a2 = out.a[2];
    frame->a3 = out.a[3];
    frame->a4 = out.a[4];
    frame->a5 = out.a[5];
    frame->sepc += 4;
    return frame;
}

static void check_meta_and_message(void)
{
    CHECK(abi_meta(0, 0) == 0);
    CHECK(abi_meta(UINT32_MAX, 0) == UINT32_MAX);
    CHECK(abi_meta(0, UINT32_MAX) == UINT64_C(0xffffffff00000000));
    CHECK(abi_meta(UINT32_MAX, UINT32_MAX) == UINT64_MAX);

    const uint64_t meta = abi_meta(UINT32_C(0x89abcdef), UINT32_C(0x76543210));
    CHECK(abi_meta_tag(meta) == UINT32_C(0x89abcdef));
    CHECK(abi_meta_reply(meta) == UINT32_C(0x76543210));

    const struct abi_message source = {
        .tag = UINT32_C(0x01020304),
        .reply = UINT32_C(0x05060708),
        .words = {
            UINT64_C(0x1112131415161718),
            UINT64_C(0x2122232425262728),
            UINT64_C(0x3132333435363738),
            UINT64_C(0x4142434445464748),
        },
    };

    struct abi_regs outbound = abi_message_args(ABI_DEMO_ENDPOINT, &source);
    CHECK(outbound.a[0] == ABI_DEMO_ENDPOINT);
    CHECK(outbound.a[1] == source.tag); /* 发送侧 a1 只是 tag，不是完整 meta */
    for (unsigned i = 0; i < ABI_WORDS; i++)
        CHECK(outbound.a[i + 2] == source.words[i]);

    struct abi_regs inbound = abi_result(ABI_OK, &source);
    struct abi_message decoded = abi_decode_message(inbound);
    CHECK(abi_decode_status(inbound) == ABI_OK);
    CHECK(decoded.tag == source.tag && decoded.reply == source.reply);
    for (unsigned i = 0; i < ABI_WORDS; i++)
        CHECK(decoded.words[i] == source.words[i]);

    struct abi_message untouched = {.tag = 123};
    CHECK(abi_decode_result(inbound, &untouched) == ABI_OK && untouched.tag == source.tag);
    untouched = (struct abi_message){.tag = 123};
    CHECK(abi_decode_result(abi_result(ABI_BAD_ARG, &source), &untouched) == ABI_BAD_ARG);
    CHECK(untouched.tag == 123);
}

static void check_status_and_shapes(void)
{
    const int64_t statuses[] = {
        ABI_OK, ABI_BAD_NR, ABI_BAD_ARG, ABI_BAD_HANDLE, ABI_DENIED,
        ABI_NO_MEMORY, ABI_CLOSED, ABI_ALREADY_MAPPED, ABI_NOT_MAPPED, ABI_BUSY,
    };
    const struct abi_message message = {.tag = 7, .reply = 9, .words = {11}};

    for (unsigned i = 0; i < sizeof(statuses) / sizeof(statuses[0]); i++) {
        struct abi_regs result = abi_result(statuses[i], &message);

        CHECK(abi_decode_status(result) == statuses[i]);
        if (statuses[i] == ABI_OK) {
            CHECK(result.a[1] == abi_meta(message.tag, message.reply));
        } else {
            CHECK(abi_tail_zero(result, 1));
        }
    }
    CHECK(abi_tail_zero(abi_result(ABI_OK, NULL), 1));

    struct abi_regs zeros = {{0}};
    CHECK(abi_tail_zero(zeros, 0));
    zeros.a[3] = 1;
    CHECK(!abi_tail_zero(zeros, 0) && !abi_tail_zero(zeros, 3));
    CHECK(abi_tail_zero(zeros, 4));

    uint32_t value = 0;
    CHECK(abi_arg_u32((struct abi_regs){{UINT32_MAX}}, 0, &value));
    CHECK(value == UINT32_MAX);
    CHECK(!abi_arg_u32((struct abi_regs){{UINT64_C(1) << 32}}, 0, &value));
    CHECK(!abi_arg_u32(zeros, ABI_REGS, &value));
    CHECK(abi_arg_u32((struct abi_regs){{7}}, 0, NULL));

    CHECK(abi_page_aligned(0));
    CHECK(abi_page_aligned(ABI_PAGE_SIZE));
    CHECK(abi_page_aligned(UINT64_C(3) * ABI_PAGE_SIZE));
    CHECK(!abi_page_aligned(ABI_PAGE_SIZE - 1));

    CHECK(abi_rights_valid(ABI_READ));
    CHECK(abi_rights_valid(ABI_READ | ABI_WRITE));
    CHECK(abi_rights_valid(ABI_READ | ABI_EXEC));
    CHECK(!abi_rights_valid(0));
    CHECK(!abi_rights_valid(ABI_WRITE)); /* 可写必须可读 */
    CHECK(!abi_rights_valid(ABI_RIGHTS_MASK + 1));
}

static void run(void)
{
    check_meta_and_message();
    check_status_and_shapes();

    const struct abi_message sent = {.tag = ABI_DEMO_TAG, .words = {ABI_DEMO_WORD}};
    struct abi_regs result = abi_ecall(ABI_IPC_SEND,
                                       abi_message_args(ABI_DEMO_ENDPOINT, &sent));
    CHECK(abi_decode_status(result) == ABI_OK);
    CHECK(abi_tail_zero(result, 1));

    result = abi_ecall(ABI_IPC_RECV, (struct abi_regs){{ABI_DEMO_ENDPOINT}});
    struct abi_message received = {0};
    CHECK(abi_decode_result(result, &received) == ABI_OK);
    CHECK(received.tag == ABI_DEMO_TAG && received.reply == ABI_DEMO_REPLY);
    CHECK(received.words[0] == ABI_DEMO_WORD);
    CHECK(received.words[1] == 0 && received.words[2] == 0 && received.words[3] == 0);
    CHECK(abi_result(ABI_OK, &received).a[1] == result.a[1]);

    struct abi_message untouched = {.tag = 123};
    result = abi_ecall(99, (struct abi_regs){{ABI_DEMO_ENDPOINT}});
    CHECK(abi_decode_result(result, &untouched) == ABI_BAD_NR);
    CHECK(untouched.tag == 123);
    CHECK(result.a[0] == UINT64_MAX && abi_tail_zero(result, 1));

    demo_log("v0 register vectors, meta, status codes and shared validation verified");
}

DEMO_MODULE(abi, true, run, handle_trap);
