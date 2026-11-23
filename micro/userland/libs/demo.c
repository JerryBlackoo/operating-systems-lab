#include "runtime.h"
#include "demo.h"
#include "csr.h"
#include "trap.h"

enum { ENDPOINT = 0xf0010002u, REPLY = 0xc0000009u };
static unsigned calls;

static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    if (frame->scause != EXC_ECALL_U || (frame->sstatus & SSTATUS_SPP))
        return NULL;
    const struct abi_message response = {.tag = 16, .reply = REPLY, .words = {8, 9, 10, 11}};
    struct abi_regs out;

    calls++;
    switch (frame->a7) {
    case ABI_IPC_SEND:
    case ABI_IPC_CALL:
        CHECK(frame->a0 == ENDPOINT && frame->a1 == UINT32_C(0xf0000007));
        CHECK(frame->a2 == 1 && frame->a3 == 2 && frame->a4 == 3 && frame->a5 == 4);
        out = frame->a7 == ABI_IPC_CALL ? abi_result(ABI_CLOSED, NULL) : abi_result(ABI_OK, NULL);
        break;
    case ABI_IPC_RECV:
        CHECK(frame->a0 == ENDPOINT && (frame->a1 | frame->a2 | frame->a3 | frame->a4 | frame->a5) == 0);
        out = abi_result(ABI_OK, &response);
        break;
    case ABI_IPC_REPLY:
        CHECK(frame->a0 == REPLY && frame->a1 == 16 && frame->a5 == 11);
        out = abi_result(ABI_OK, NULL);
        break;
    case ABI_THREAD_EXIT:
        CHECK(calls == 5 && (frame->a0 | frame->a1 | frame->a2 | frame->a3 | frame->a4 | frame->a5) == 0);
        demo_pass();
    default:
        demo_fail("unexpected syscall", __LINE__);
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

static void app_main(const struct start_info *info)
{
    uintptr_t sp;
    __asm__ volatile("mv %0, sp" : "=r"(sp));
    CHECK((sp & 15) == 0 && info->argument == 123);

    const struct abi_message out = {.tag = 0xf0000007u, .words = {1, 2, 3, 4}};
    struct abi_message in = {0};
    CHECK(user_ipc_send(ENDPOINT, &out) == ABI_OK);
    CHECK(user_ipc_recv(ENDPOINT, &in) == ABI_OK);
    CHECK(in.tag == 16 && in.reply == REPLY && in.words[0] == 8 && in.words[3] == 11);
    CHECK(user_ipc_reply(in.reply, &in) == ABI_OK);
    CHECK(user_ipc_call(ENDPOINT, &out, &in) == ABI_CLOSED);
    CHECK(in.tag == 16 && in.reply == REPLY && in.words[0] == 8 && in.words[3] == 11);
    demo_log("runtime: stack alignment, IPC wrappers, signed error and thread exit");
}

static void run(void)
{
    const struct start_info info = {.argument = 123};
    user_start(&info, app_main);
}

DEMO_MODULE(libs, true, run, handle_trap);
