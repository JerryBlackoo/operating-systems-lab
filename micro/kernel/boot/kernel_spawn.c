/*
 * kernel_spawn.c — PM 在用户态通过 ABI_PM_SPAWN 创建 initrd 子进程（M3/M7）。
 *
 * M6 起 VM 也由 init 经 PM spawn；SID 使用固定映射而不是按 spawn 顺序自增，
 * 这样 PM 的进程表、SM 的能力源和 pager 的调用者检查不会随启动顺序漂移。
 */
#include "kernel_spawn.h"

#include "cap_as.h"
#include "abi/abi.h"
#include "demo.h"
#include "loader/loader.h"
#include "object.h"
#include "sched.h"
#include "service_boot.h"

static struct bootinfo spawn_bi_storage;
static const struct bootinfo *spawn_bi;
static pfn_t spawn_root;
static uint64_t spawn_next_pid = 10;

static const char *image_name(uint32_t image)
{
    switch (image) {
    case SPAWN_FS:
        return "fs";
    case SPAWN_CONSOLE:
        return "console";
    case SPAWN_SHELL:
        return "shell";
    case SPAWN_VM:
        return "vm";
    case SPAWN_HEAPTEST:
        return "heaptest";
    case SPAWN_HEAPTEST2:
        return "heaptest2";
    default:
        return 0;
    }
}

static cap_sid_t image_sid(uint32_t image)
{
    switch (image) {
    case SPAWN_FS:
        return SERVICE_SID_FS;
    case SPAWN_CONSOLE:
        return SERVICE_SID_CONSOLE;
    case SPAWN_SHELL:
        return SERVICE_SID_SHELL;
    case SPAWN_VM:
        return SERVICE_SID_VM;
    case SPAWN_HEAPTEST:
        return SERVICE_SID_HEAPTEST;
    case SPAWN_HEAPTEST2:
        return SERVICE_SID_HEAPTEST2;
    default:
        return 0;
    }
}

static int64_t cap_rc_to_abi(int rc)
{
    switch (rc) {
    case CAP_OK:
        return ABI_OK;
    case CAP_E_NOMEM:
        return ABI_NO_MEMORY;
    case CAP_E_RIGHTS:
    case CAP_E_TYPE:
        return ABI_DENIED;
    default:
        return ABI_NO_MEMORY;
    }
}

static int64_t loader_error_to_abi(int rc)
{
    switch (rc) {
    case LOADER_OK:
        return ABI_OK;
    case LOADER_ENOENT:
        return ABI_NOT_FOUND;
    case LOADER_ENOMEM:
    case LOADER_EFULL:
        return ABI_NO_MEMORY;
    case LOADER_EINVAL:
    case LOADER_EMAP:
    default:
        return ABI_BAD_ARG;
    }
}

void kernel_spawn_init(const struct bootinfo *bi, pfn_t kernel_root)
{
    /*
     * boot_main 的 BootInfo 在启动栈上；PM 之后的 spawn 仍要读 initrd
     * 区间，因此这里保存一份静态副本，不依赖启动栈的寿命。
     */
    if (bi != 0) {
        spawn_bi_storage = *bi;
        spawn_bi = &spawn_bi_storage;
    } else {
        spawn_bi = 0;
    }
    spawn_root = kernel_root;
    spawn_next_pid = 10;
}

/*
 * 安装到目标 SID 的 cap 在 spawn 失败时必须撤回，否则固定 SID 的
 * CSpace 会残留半套 cap：重试时槽位冲突，且在 64 槽耗尽后永久失败。
 */
static void rollback_spawn_caps(cap_sid_t sid, cap_handle_t ready_h,
                                cap_handle_t recv_h, cap_handle_t sm_h)
{
    if (recv_h != 0) {
        (void)cap_delete(sid, recv_h);
    }
    if (sm_h != 0) {
        (void)cap_delete(sid, sm_h);
    }
    if (ready_h != 0) {
        (void)cap_delete(sid, ready_h);
    }
}

int64_t kernel_cap_mint_to(uint8_t sm_sid, cap_handle_t src, cap_sid_t dst_sid,
                           cap_rights_t rights, cap_badge_t badge,
                           cap_handle_t *out)
{
    if (out == 0 || sm_sid != SERVICE_SID_SM) {
        return ABI_DENIED;
    }
    if (cap_mint_to((cap_sid_t)sm_sid, src, (cap_sid_t)dst_sid, CAP_SLOT_AUTO,
                    rights, badge, out) != CAP_OK) {
        return ABI_DENIED;
    }
    return ABI_OK;
}

int64_t kernel_pm_spawn(uint8_t caller_sid, uint64_t parent, uint32_t image,
                        cap_rights_t reserved, uint64_t *pid_out,
                        uint64_t *tid_out, cap_handle_t *pm_as_cap_out)
{
    const char *name;
    struct loader_boot_args boot = {0};
    cap_handle_t ready_h = 0;
    cap_handle_t recv_h = 0;
    cap_handle_t sm_h = 0;
    struct thread *child = 0;
    cap_sid_t sid;
    uint64_t receipt;
    int rc;

    /*
     * reserved（ABI a2）当前不使用：spawn 的 cap 组合由内核固定，
     * 不允许用户态扩大。保留形参只为 ABI 形状稳定。
     */
    (void)reserved;
    if (caller_sid != SERVICE_SID_PM) {
        return ABI_DENIED;
    }
    if (pid_out == 0) {
        return ABI_BAD_ARG;
    }
    if (spawn_bi == 0) {
        return ABI_NO_MEMORY;
    }
    name = image_name(image);
    sid = image_sid(image);
    if (name == 0 || sid == 0) {
        return ABI_BAD_ARG;
    }

    receipt = 0x2000u + image;
    if (service_stub_ready_send(sid, receipt, &ready_h) != 0 ||
        service_sm_install_send(sid, &sm_h) != 0) {
        demo_log("[pm] spawn: cap install failed");
        rollback_spawn_caps(sid, ready_h, 0, sm_h);
        return ABI_NO_MEMORY;
    }

    switch (image) {
    case SPAWN_FS:
        if (service_fs_install_service_recv(&recv_h) != 0) {
            rollback_spawn_caps(sid, ready_h, 0, sm_h);
            return ABI_NO_MEMORY;
        }
        boot = (struct loader_boot_args){
            .cap_sid = (uint8_t)sid,
            .a0 = ready_h,
            .a1 = receipt,
            .a2 = recv_h,
            .a3 = sid,
            .a4 = sm_h,
        };
        break;
    case SPAWN_CONSOLE:
        if (service_console_install_service_recv(&recv_h) != 0) {
            rollback_spawn_caps(sid, ready_h, 0, sm_h);
            return ABI_NO_MEMORY;
        }
        boot = (struct loader_boot_args){
            .cap_sid = (uint8_t)sid,
            .a0 = ready_h,
            .a1 = receipt,
            .a2 = recv_h,
            .a3 = sid,
            .a4 = sm_h,
        };
        break;
    case SPAWN_VM:
        if (service_vm_install_service_recv(&recv_h) != 0) {
            rollback_spawn_caps(sid, ready_h, 0, sm_h);
            return ABI_NO_MEMORY;
        }
        /* vm_start(recv, ready, receipt, sm_send, sid)：a3/a4 顺序不能反。 */
        boot = (struct loader_boot_args){
            .cap_sid = (uint8_t)sid,
            .a0 = recv_h,
            .a1 = ready_h,
            .a2 = receipt,
            .a3 = sm_h,
            .a4 = sid,
        };
        break;
    case SPAWN_SHELL:
        boot = (struct loader_boot_args){
            .cap_sid = (uint8_t)sid,
            .a0 = ready_h,
            .a1 = receipt,
            .a2 = sm_h,
            .a3 = sid,
        };
        break;
    case SPAWN_HEAPTEST:
    case SPAWN_HEAPTEST2:
        boot = (struct loader_boot_args){
            .cap_sid = (uint8_t)sid,
            .a0 = ready_h,
            .a1 = receipt,
            .a2 = sm_h,
            .a3 = sid,
            .a4 = image == SPAWN_HEAPTEST2 ? 1u : 0u,
        };
        break;
    default:
        rollback_spawn_caps(sid, ready_h, recv_h, sm_h);
        return ABI_BAD_ARG;
    }

    rc = loader_spawn_user(spawn_bi, spawn_root, name, &boot, &child);
    if (rc != LOADER_OK) {
        demo_log("[pm] spawn: loader rejected image");
        /* loader 失败不会创建线程，已装的 cap 必须全部撤回。 */
        rollback_spawn_caps(sid, ready_h, recv_h, sm_h);
        return loader_error_to_abi(rc);
    }

    /*
     * pid 必须在所有失败路径之后才消耗：先前实现在 cap 安装前就
     * `spawn_next_pid++`，cap 安装失败时 pid 已被吃掉，`ps` 里会出现
     * 跳号，而且 PM 可能拿到一个已被 abort 的线程对应的 pid。
     */
    if (pm_as_cap_out != 0) {
        cap_handle_t pm_as = 0;

        *pm_as_cap_out = 0;
        if (child != 0 && child->as != 0) {
            int cap_rc = cap_pm_install_as_grant(child->as, &pm_as);

            if (cap_rc != CAP_OK) {
                demo_log("[pm] spawn: as cap install failed");
                /*
                 * 与 ABI_PM_DISCARD 同序：先撤回子线程，成功才拆 cap。
                 * 若子线程已 RUNNING（abort 失败），保留 cap 让 PM 仍能
                 * 通过正常路径收尾，而不是造出「活着但无法注册」的孤儿。
                 */
                if (loader_abort_spawn(child) == 0) {
                    rollback_spawn_caps(sid, ready_h, recv_h, sm_h);
                }
                return cap_rc_to_abi(cap_rc);
            }
            *pm_as_cap_out = pm_as;
        }
    }
    /*
     * 所有可能失败的步骤都过了，现在才分配 pid 并回报 thread id。
     * 这样 PM 拿到的 pid 一定对应一个已经完整建立的子进程。
     */
    if (child == 0) {
        /* loader 成功但没回线程：视为内部错误，不消耗 pid。 */
        demo_log("[pm] spawn: loader returned no thread");
        rollback_spawn_caps(sid, ready_h, recv_h, sm_h);
        return ABI_NO_MEMORY;
    }
    *pid_out = spawn_next_pid++;
    if (tid_out != 0) {
        *tid_out = object_get_id(&child->obj);
    }
    /*
     * VM 的 MAP cap 由 PM 用 ABI_VM_GRANT_AS + cap_mint_to 交给 VM；
     * 内核直 spawn 在 kernel_init 里 cap_vm_ensure_as_map。
     */
    (void)parent;
    return ABI_OK;
}

static bool image_has_service_recv(uint32_t image)
{
    switch (image) {
    case SPAWN_FS:
    case SPAWN_CONSOLE:
    case SPAWN_VM:
        return true;
    default:
        return false;
    }
}

int64_t kernel_pm_discard(uint8_t caller_sid, uint32_t thread_id,
                          uint32_t image)
{
    struct object *obj;
    struct thread *target;
    cap_sid_t sid;
    int rc;

    if (caller_sid != SERVICE_SID_PM) {
        return ABI_DENIED;
    }
    if (image_name(image) == 0) {
        return ABI_BAD_ARG;
    }
    obj = object_lookup(thread_id, OBJ_THREAD);
    if (obj == 0) {
        return ABI_BAD_ARG;
    }
    target = (struct thread *)obj;
    sid = (cap_sid_t)sched_thread_cap_sid(target);
    if (sid != image_sid(image)) {
        object_unref(obj);
        return ABI_BAD_ARG;
    }
    /*
     * 先尝试终止再拆 cap：终止失败时不能把仍在运行的子进程的
     * ready/sm/recv cap 抽走，否则留下一个「活着但无法注册、无法收请求」
     * 的孤儿。
     *
     * 走 loader_terminate_spawn() → sched_thread_terminate()：RUNNING 的
     * 线程收到延迟终止请求并在下次进入内核时退出，阻塞在异步 syscall 上的
     * 线程先撤销在途 reply 再摘除，因此原先必须回 ABI_BUSY 的尾巴已消掉；
     * 只有 current/idle 这类不可终止目标才返回 ABI_BUSY。
     */
    rc = loader_terminate_spawn(target);
    if (rc != 0) {
        object_unref(obj);
        return ABI_BUSY;
    }
    service_teardown_child_caps(sid, image_has_service_recv(image));
    object_unref(obj);
    return ABI_OK;
}