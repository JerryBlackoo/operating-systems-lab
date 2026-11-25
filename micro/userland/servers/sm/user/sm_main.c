#include "abi/abi.h"
#include "runtime.h"
#include "service_proto.h"
#include "user_io.h"

struct sm_entry {
    const char *name;
    uint32_t sid;
    uint32_t source;
    int live;
};

static struct sm_entry sm_entries[] = {
    {"pm", USER_SID_PM, 0, 0},
    {"fs", USER_SID_FS, 0, 0},
    {"console", USER_SID_CONSOLE, 0, 0},
    /* M6：heaptest 通过 SM 拿到 VM endpoint 的发送句柄。 */
    {"vm", USER_SID_VM, 0, 0},
};

static struct sm_entry *find_entry(uint64_t name)
{
    for (unsigned i = 0; i < sizeof(sm_entries) / sizeof(sm_entries[0]); i++) {
        if (svc_name_equals(name, sm_entries[i].name)) {
            return &sm_entries[i];
        }
    }
    return 0;
}

static void reply_ok(uint32_t reply, uint64_t word)
{
    struct abi_message msg = {
        .tag = TAG_SM_OK,
        .words = {word, 0, 0, 0},
    };

    (void)user_ipc_reply(reply, &msg);
}

static void reply_err(uint32_t reply, int64_t status)
{
    struct abi_message msg = {
        .tag = TAG_SM_ERROR,
        .words = {(uint64_t)status, 0, 0, 0},
    };

    (void)user_ipc_reply(reply, &msg);
}

void sm_start(uint32_t recv_ep, uint32_t pm_src, uint32_t fs_src,
              uint32_t console_src, uint32_t vm_src) __attribute__((used));
void sm_start(uint32_t recv_ep, uint32_t pm_src, uint32_t fs_src,
              uint32_t console_src, uint32_t vm_src)
{
    sm_entries[0].source = pm_src;
    sm_entries[1].source = fs_src;
    sm_entries[2].source = console_src;
    sm_entries[3].source = vm_src;

    user_print("[sm] serving");

    for (;;) {
        struct abi_message msg = {0};
        struct sm_entry *e;

        if (user_ipc_recv(recv_ep, &msg) != ABI_OK) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
            continue;
        }
        if (msg.reply == 0) {
            continue;
        }
        if (msg.tag == TAG_SM_REGISTER) {
            uint32_t claimed_sid = (uint32_t)msg.words[2];
            uint8_t real_sid = 0;
            uint32_t sid;

            /*
             * 身份只认内核给的：msg.words[2] 是调用者自己填的，任何 U-mode
             * 进程都能写成别人的 sid。用 reply_id 向内核查真实 cap_sid，
             * 查不到就拒绝，绝不回退到自称值。
             */
            if (user_reply_caller_sid(msg.reply, &real_sid) != ABI_OK) {
                reply_err(msg.reply, ABI_DENIED);
                continue;
            }
            sid = real_sid;
            if (sid == 0 || sid >= USER_SID_MAX) {
                reply_err(msg.reply, ABI_DENIED);
                continue;
            }
            e = find_entry(msg.words[1]);
            if (e == 0 || e->sid != sid || claimed_sid != sid) {
                reply_err(msg.reply, ABI_NOT_FOUND);
                continue;
            }
            e->live = 1;
            user_print_u64("[sm] register sid=", sid, "");
            reply_ok(msg.reply, 0);
        } else if (msg.tag == TAG_SM_LOOKUP) {
            uint32_t claimed_sid = (uint32_t)msg.words[2];
            uint8_t real_sid = 0;
            uint32_t sid;
            uint32_t handle = 0;

            /*
             * 目标 sid 必须来自内核。否则攻击者只要把 words[2] 填成
             * console/shell 的 sid，就能让 SM 把 endpoint 发送权 mint 进
             * 别人的 CSpace（槽位污染），或冒用他人身份取句柄。
             */
            if (user_reply_caller_sid(msg.reply, &real_sid) != ABI_OK) {
                reply_err(msg.reply, ABI_DENIED);
                continue;
            }
            sid = real_sid;
            if (claimed_sid != sid) {
                reply_err(msg.reply, ABI_DENIED);
                continue;
            }
            e = find_entry(msg.words[1]);
            if (e == 0 || !e->live) {
                reply_err(msg.reply, ABI_NOT_FOUND);
                continue;
            }
            if (sid == 0 || sid >= USER_SID_MAX ||
                user_cap_mint_to(e->source, sid, SVC_CAP_WRITE, 0, &handle) !=
                    ABI_OK) {
                reply_err(msg.reply, ABI_DENIED);
                continue;
            }
            user_print_u64("[sm] lookup sid=", sid, "");
            reply_ok(msg.reply, handle);
        } else {
            reply_err(msg.reply, ABI_BAD_ARG);
        }
    }
}