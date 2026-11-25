#include "abi/abi.h"
#include "runtime.h"
#include "service_proto.h"
#include "user_io.h"

#define PM_MAX_PROCS 16

struct pm_proc {
    uint64_t pid;
    char name[12];
    uint32_t state;
    uint32_t image;
};

static struct pm_proc pm_procs[PM_MAX_PROCS];
static unsigned pm_proc_count;

static const char *image_name(uint32_t image)
{
    switch (image) {
    case 1:
        return "fs";
    case 2:
        return "console";
    case 3:
        return "shell";
    case 4:
        return "vm";
    case 5:
        return "heaptest";
    case 6:
        return "heaptest2";
    default:
        return 0;
    }
}

static int add_proc(uint64_t pid, const char *name, uint32_t image)
{
    struct pm_proc *p;

    if (name == 0 || pm_proc_count >= PM_MAX_PROCS) {
        return -1;
    }
    p = &pm_procs[pm_proc_count++];
    p->pid = pid;
    p->state = 1;
    p->image = image;
    for (unsigned i = 0; i < sizeof(p->name) - 1U && name[i] != '\0'; i++) {
        p->name[i] = name[i];
    }
    p->name[sizeof(p->name) - 1U] = '\0';
    return 0;
}

static void reply_msg(uint32_t reply, uint32_t tag, uint64_t w0, uint64_t w1,
                      uint64_t w2, uint64_t w3){
    struct abi_message msg = {
        .tag = tag,
        .words = {w0, w1, w2, w3},
    };

    (void)user_ipc_reply(reply, &msg);
}

/*
 * 通知 VM 丢弃某个已终止线程的全部 region/驻留页。
 *
 * 用 call（不是 send）：VM 需要靠 reply token 向内核查发送者真实 sid，
 * 确认是 PM 才执行——否则任何持有 VM 发送能力的进程（heaptest 等）都能
 * 清掉别人的地址空间区间。VM 的处理是纯内存操作（逐页 unmap + 清 region
 * 槽），不会长时间阻塞 PM。
 *
 * 失败不影响 spawn 结果：少一次回收只是浪费可复用槽位，不是正确性问题。
 */
static void vm_forget(uint32_t vm_send, uint64_t tid)
{
    struct abi_message req = {
        .tag = TAG_VM_FORGET,
        .words = {tid, 0, 0, 0},
    };
    struct abi_message rep = {0};

    if (vm_send == 0) {
        return;
    }
    (void)user_ipc_call(vm_send, &req, &rep);
}

static void register_to_sm(uint32_t sm_send, uint32_t sid)
{
    struct abi_message req = {
        .tag = TAG_SM_REGISTER,
        .words = {0, svc_name_pack("pm"), sid, 0},
    };

    for (;;) {
        struct abi_message rep = {0};

        if (user_ipc_call(sm_send, &req, &rep) == ABI_OK &&
            rep.tag == TAG_SM_OK) {
            user_print("[pm] registered with sm");
            return;
        }
        abi_ecall(ABI_YIELD, (struct abi_regs){0});
    }
}

void pm_start(uint32_t recv_ep, uint32_t sm_send, uint32_t sid, uint32_t vm_send)
    __attribute__((used));
void pm_start(uint32_t recv_ep, uint32_t sm_send, uint32_t sid, uint32_t vm_send)
{
    (void)add_proc(1, "init", 0);
    (void)add_proc(2, "sm", 0);
    (void)add_proc(3, "pm", 0);
    register_to_sm(sm_send, sid);

    for (;;) {
        struct abi_message msg = {0};

        if (user_ipc_recv(recv_ep, &msg) != ABI_OK) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
            continue;
        }
        if (msg.reply == 0) {
            continue;
        }

        if (msg.tag == TAG_PM_SPAWN) {
            uint32_t image = (uint32_t)msg.words[2];
            const char *name = image_name(image);

            /*
             * 先检查表容量再调内核 spawn；否则进程已经拉起、PM 却
             * 记不进去，会留下不受进程表管理的服务。
             */
            if (pm_proc_count >= PM_MAX_PROCS) {
                reply_msg(msg.reply, TAG_PM_ERROR, (uint64_t)ABI_NO_MEMORY,
                          0, 0, 0);
                continue;
            }
            struct abi_regs in = {{
                msg.words[1], msg.words[2], msg.words[3], 0, 0, 0,
            }};
            struct abi_message out = {0};
            int64_t st = abi_decode_result(abi_ecall(ABI_PM_SPAWN, in), &out);

            if (st != ABI_OK || name == 0) {
                reply_msg(msg.reply, TAG_PM_ERROR, (uint64_t)st, 0, 0, 0);
                continue;
            }
            /*
             * 内核只在子进程确实有地址空间时才回一个 AS 源 cap（words[2]）。
             * 若 words[1]（thread id）为空，说明 spawn 没有产出可用线程，
             * 此时绝不能再往进程表里登记一条记录——那会是一个永远无法
             * 运行、也无法被 discard 的幽灵进程。
             */
            if (out.words[1] == 0) {
                user_print("[pm] spawn: kernel returned no thread id");
                reply_msg(msg.reply, TAG_PM_ERROR, (uint64_t)ABI_NO_MEMORY,
                          0, 0, 0);
                continue;
            }
            if (out.words[2] == 0) {
                /*
                 * 没有 AS 源 cap 时无法把补页权交给 VM。当前所有服务镜像
                 * 都有 AS，走到这里说明装配出了问题——记录明确日志，但仍
                 * 允许进程登记（它只是日后缺页会被拒）。
                 */
                user_print("[pm] spawn: no AS cap, VM grant skipped");
            } else {
                int64_t gst;

                /*
                 * PM 显式把新进程 AS 的 MAP 权交给 VM（M6 cap 模型）。
                 * words[0]=展示 pid，words[1]=内核 thread object id。
                 */
                struct abi_message grant_out = {0};

                gst = abi_decode_result(
                    abi_ecall(ABI_VM_GRANT_AS,
                              (struct abi_regs){
                                  {out.words[1], out.words[2], 0, 0, 0, 0}}),
                    &grant_out);
                if (gst != ABI_OK) {
                    (void)abi_decode_result(
                        abi_ecall(ABI_CAP_DELETE,
                                  (struct abi_regs){
                                      {out.words[2], 0, 0, 0, 0, 0}}),
                        0);
                    (void)abi_decode_result(
                        abi_ecall(ABI_PM_DISCARD,
                                  (struct abi_regs){
                                      {out.words[1], image, 0, 0, 0, 0}}),
                        0);
                    vm_forget(vm_send, out.words[1]);
                    reply_msg(msg.reply, TAG_PM_ERROR, (uint64_t)gst, 0, 0, 0);
                    continue;
                }
                user_print_u64("[pm] vm grant as=", grant_out.words[0], "");
            }
            if (add_proc(out.words[0], name, image) != 0) {
                (void)abi_decode_result(
                    abi_ecall(ABI_PM_DISCARD,
                              (struct abi_regs){
                                  {out.words[1], image, 0, 0, 0, 0}}),
                    0);
                vm_forget(vm_send, out.words[1]);
                reply_msg(msg.reply, TAG_PM_ERROR, (uint64_t)ABI_NO_MEMORY,
                          0, 0, 0);
                continue;
            }
            user_print_u64("[pm] spawn pid=", out.words[0], "");
            reply_msg(msg.reply, TAG_PM_SPAWN_OK, out.words[0], 0, 0, 0);
        } else if (msg.tag == TAG_PM_PS) {
            uint64_t index = msg.words[0];

            if (index >= pm_proc_count) {
                reply_msg(msg.reply, TAG_PM_ERROR, (uint64_t)ABI_NOT_FOUND,
                          0, 0, 0);
                continue;
            }
            reply_msg(msg.reply, TAG_PM_PS_OK, pm_procs[index].pid,
                      svc_name_pack(pm_procs[index].name),
                      pm_procs[index].state, pm_proc_count);
        } else {
            reply_msg(msg.reply, TAG_PM_ERROR, (uint64_t)ABI_BAD_ARG, 0, 0, 0);
        }
    }
}