#include "abi/abi.h"
#include "runtime.h"
#include "service_proto.h"
#include "user_io.h"

static uint32_t ready_mask;

static int sm_lookup(uint32_t sm_send, const char *name, uint32_t sid,
                     uint32_t *handle)
{
    struct abi_message req = {
        .tag = TAG_SM_LOOKUP,
        .words = {0, svc_name_pack(name), sid, 0},
    };

    for (;;) {
        struct abi_message rep = {0};

        if (user_ipc_call(sm_send, &req, &rep) == ABI_OK &&
            rep.tag == TAG_SM_OK && rep.words[0] != 0) {
            *handle = (uint32_t)rep.words[0];
            return 0;
        }
        abi_ecall(ABI_YIELD, (struct abi_regs){0});
    }
}

static int pm_spawn(uint32_t pm, uint32_t image, uint64_t *pid, int64_t *status)
{
    struct abi_message req = {
        .tag = TAG_PM_SPAWN,
        .words = {SVC_SPAWN_OP, 100, image, ABI_READ},
    };
    struct abi_message rep = {0};
    int64_t st = user_ipc_call(pm, &req, &rep);

    if (st != ABI_OK) {
        *status = st;
        return -1;
    }
    if (rep.tag != TAG_PM_SPAWN_OK) {
        *status = rep.tag == TAG_PM_ERROR ? (int64_t)rep.words[0] : ABI_BAD_ARG;
        return -1;
    }
    *pid = rep.words[0];
    *status = ABI_OK;
    return 0;
}

static void print_spawn_failed(uint32_t image, int64_t status)
{
    user_print_u64("[init] spawn failed image=", image, "");
    if (status < 0) {
        user_print_u64("[init] spawn failed status=-", (uint64_t)(-status), "");
    } else {
        user_print_u64("[init] spawn failed status=", (uint64_t)status, "");
    }
}

/*
 * init 的启动分两阶段，顺序是被刻意安排的：
 *
 *   1. 拉起 fs 与 console，等它们都回执 ready；
 *   2. 才拉起 shell。
 *
 * 为什么不能在同一个循环里全拉起来：shell 一启动就立刻打印 `micro> `
 * 并开始读命令行，而 fs 的只读自检（挂载 + 读 /motd 与 /largefile 算
 * SHA-256）需要时间。两者并行时，fs 的分块输出会插进 shell 的
 * `commands: help cat ps echo poweroff` 中间，把这行拆成两段——用户看到
 * 的输出被破坏，M5 判别与 demo 的整行匹配也会间歇性失败。
 *
 * 阶段 2 的 shell 必须等阶段 1 的 ready 全部到齐，这是唯一的顺序保证点。
 */
enum { INIT_IMAGE_FS = 1, INIT_IMAGE_CONSOLE = 2, INIT_IMAGE_SHELL = 3 };

void init_main(uint32_t recv_only, uint32_t sm_send, uint32_t ready_ep,
               uint32_t plan, uint32_t sid)
{
    uint32_t pm = 0;
    /* 启动顺序：服务端（fs/console）先，交互端（shell）最后。 */
    static const uint32_t phases[][3] = {
        {INIT_IMAGE_FS, INIT_IMAGE_CONSOLE, 0},
        {INIT_IMAGE_SHELL, 0, 0},
    };
    static const char *const names[] = {"fs", "console", "shell"};

    (void)recv_only;
    if (plan > 3u) {
        plan = 3u;
    }
    user_print_u64("[init] boot: plan=", plan, "");
    user_print_u64("[init] sid=", sid, "");

    sm_lookup(sm_send, "pm", sid, &pm);

    uint32_t spawned = 0;
    uint32_t spawned_mask = 0;

    for (unsigned phase = 0; phase < 2; phase++) {
        for (unsigned i = 0; i < 3; i++) {
            uint32_t image = phases[phase][i];
            uint64_t pid = 0;
            int64_t status = ABI_OK;

            if (image == 0 || spawned >= plan) {
                continue;
            }
            if (pm_spawn(pm, image, &pid, &status) != 0) {
                print_spawn_failed(image, status);
                for (;;) {
                    abi_ecall(ABI_YIELD, (struct abi_regs){0});
                }
            }
            user_print_u64("[init] spawn ok pid=", pid, "");
            user_print(names[image - 1]);
            spawned++;
            spawned_mask |= 1u << image;
        }

        /*
         * 等本阶段所有已 spawn 的服务回执 ready，再进入下一阶段。
         * 阶段 0 结束时，fs 的自检输出已经全部落地，shell 才会开始
         * 打印提示符。
         *
         * 就绪位按 **image 号** 记（receipt = 0x2000 + image），不是按
         * spawn 序号：阶段 0 的 fs/console 是 image 1/2，对应 bit1/bit2。
         * 用序号会在第二个阶段把位序算错。
         */
        while ((ready_mask & spawned_mask) != spawned_mask) {
            struct abi_message msg = {0};
            uint32_t image;

            if (user_ipc_recv(ready_ep, &msg) != ABI_OK ||
                msg.tag != TAG_READY) {
                abi_ecall(ABI_YIELD, (struct abi_regs){0});
                continue;
            }
            if (msg.words[0] < 0x2001 || msg.words[0] >= 0x2001 + plan) {
                continue;
            }
            image = (uint32_t)(msg.words[0] - 0x2001) + 1u;
            ready_mask |= 1u << image;
        }
    }
    user_print("[init] ready");

    for (;;) {
        struct abi_message msg = {0};

        if (user_ipc_recv(ready_ep, &msg) != ABI_OK) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
        }
    }
}