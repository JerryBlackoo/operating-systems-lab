#include "abi.h"
#include "demo.h"
#include "csr.h"
#include "trap.h"

static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    if (frame->scause != EXC_ECALL_U || (frame->sstatus & SSTATUS_SPP))
        return NULL;
    CHECK(frame->a0 == 0x10002 && (frame->a3 | frame->a4 | frame->a5) == 0);
    if (frame->a7 == ABI_IPC_SEND) {
        CHECK(frame->a1 == 7 && frame->a2 == 11);
        frame->a1 = 0;
        frame->a2 = 0;
    } else {
        CHECK(frame->a7 == ABI_IPC_RECV && frame->a1 == 0 && frame->a2 == 0);
        frame->a1 = UINT64_C(0x0000000900000007);
        frame->a2 = 11;
    }
    frame->a0 = 0;
    frame->sepc += 4;
    return frame;
}

static void run(void)
{
    const struct abi_message sent = {.tag = 7, .words = {11}};
    struct abi_regs reply = abi_ecall(ABI_IPC_SEND, abi_message_args(0x10002, &sent));
    for (unsigned i = 0; i < ABI_REGS; i++)
        CHECK(reply.a[i] == 0);
    reply = abi_ecall(ABI_IPC_RECV, (struct abi_regs){{0x10002}});
    struct abi_message received = abi_decode_message(reply);

    CHECK(abi_decode_status(reply) == ABI_OK);
    CHECK(received.tag == 7 && received.reply == 9 && received.words[0] == 11);
    CHECK(abi_result(ABI_OK, &received).a[1] == reply.a[1]);
    struct abi_regs error = abi_result(ABI_BAD_NR, &received);
    CHECK(error.a[0] == UINT64_MAX && abi_decode_status(error) == ABI_BAD_NR);
    for (unsigned i = 1; i < ABI_REGS; i++)
        CHECK(error.a[i] == 0);
    demo_log("U-mode ecall and RV64 ABI register vectors verified");
}

DEMO_MODULE(abi, true, run, handle_trap);
