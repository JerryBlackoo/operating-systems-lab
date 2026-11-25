/*
 * service_boot.c — M3/M7 常驻路径：init / SM / PM / 服务 endpoint 能力。
 *
 * 内核 spawn vm/heaptest/heaptest2；init→PM 拉 fs/console/shell；PM spawn
 * 仍可用于其余服务（recv 在 kernel_pm_spawn 时安装）。
 */
#include "service_boot.h"

#include "cap_as.h"
#include "cap_endpoint.h"
#include "capability.h"
#include "demo.h"
#include "ipc/ipc.h"
#include "pager/pager.h"
#include "syscall/user_backend.h"

enum {
    SLOT_SM_RECV = 1,
    SLOT_SM_PM = 2,
    SLOT_SM_FS = 3,
    SLOT_SM_CONSOLE = 4,
    /* M6：SM 持有的 VM endpoint 源能力，用于把 vm 句柄 mint 给 heaptest。 */
    SLOT_SM_VM = 5,
    SLOT_PM_RECV = 1,
    SLOT_PM_SM = 2,
    /*
     * PM 持有一个到 VM 的发送句柄：子进程被 discard 后要通知 VM 释放该
     * tid 的 region/驻留页，否则这些槽位一直占着（VM 看不到进程退出）。
     */
    SLOT_PM_VM = 3,
    SLOT_INIT_SM_RECV = 1,
    SLOT_INIT_SM_SEND = 2,
    SLOT_INIT_READY = 3,
    SLOT_CHILD_READY = 1,
    SLOT_CHILD_RECV = 2,
    SLOT_CHILD_SM = 3,
    SLOT_HEAP_VM = 1,
};

static struct endpoint *ep_sm;
static struct endpoint *ep_pm;
static struct endpoint *ep_fs;
static struct endpoint *ep_console;
static struct endpoint *ep_vm;
static struct endpoint *ep_init_ready;
static cap_oid_t sm_oid;
static cap_oid_t pm_oid;
static cap_oid_t fs_oid;
static cap_oid_t console_oid;
static cap_oid_t vm_oid;
static cap_oid_t init_ready_oid;

static int install_root(cap_sid_t sid, cap_slot_t slot, cap_oid_t oid,
                        cap_rights_t rights, cap_badge_t badge,
                        cap_handle_t *out)
{
    if (cap_root_install(sid, slot, oid, rights, badge, out) != CAP_OK) {
        return -1;
    }
    return 0;
}

int service_stub_ready_send(cap_sid_t sid, uint64_t receipt, cap_handle_t *send_out)
{
    if (send_out == 0 || init_ready_oid == 0) {
        return -1;
    }
    return install_root(sid, SLOT_CHILD_READY, init_ready_oid, CAP_RIGHT_WRITE,
                        (cap_badge_t)receipt, send_out);
}

int service_sm_install_send(cap_sid_t sid, cap_handle_t *send_out)
{
    if (send_out == 0 || sm_oid == 0) {
        return -1;
    }
    return install_root(sid, SLOT_CHILD_SM, sm_oid, CAP_RIGHT_WRITE, 0, send_out);
}

int service_fs_install_service_recv(cap_handle_t *recv_out)
{
    if (recv_out == 0 || fs_oid == 0) {
        return -1;
    }
    return install_root(SERVICE_SID_FS, SLOT_CHILD_RECV, fs_oid, CAP_RIGHT_READ,
                        0, recv_out);
}

int service_console_install_service_recv(cap_handle_t *recv_out)
{
    if (recv_out == 0 || console_oid == 0) {
        return -1;
    }
    return install_root(SERVICE_SID_CONSOLE, SLOT_CHILD_RECV, console_oid,
                        CAP_RIGHT_READ, 0, recv_out);
}

int service_vm_install_service_recv(cap_handle_t *recv_out)
{
    if (recv_out == 0 || vm_oid == 0) {
        return -1;
    }
    return install_root(SERVICE_SID_VM, SLOT_CHILD_RECV, vm_oid, CAP_RIGHT_READ,
                        0, recv_out);
}

void service_teardown_child_caps(cap_sid_t sid, bool has_service_recv)
{
    (void)cap_delete_slot_if_present(sid, SLOT_CHILD_READY);
    (void)cap_delete_slot_if_present(sid, SLOT_CHILD_SM);
    if (has_service_recv) {
        (void)cap_delete_slot_if_present(sid, SLOT_CHILD_RECV);
    }
}

int service_boot_prepare(const struct bootinfo *bi, pfn_t kernel_root,
                         struct service_boot_layout *out)
{
    cap_handle_t sm_recv;
    cap_handle_t sm_pm;
    cap_handle_t sm_fs;
    cap_handle_t sm_console;
    cap_handle_t sm_vm;
    cap_handle_t pm_recv;
    cap_handle_t pm_sm;
    cap_handle_t pm_vm;
    cap_handle_t init_sm_recv;
    cap_handle_t init_sm_send;
    cap_handle_t init_ready;
    cap_handle_t vm_recv;
    cap_handle_t vm_sm;
    cap_handle_t heap_vm;
    cap_handle_t heap2_vm;

    (void)bi;
    (void)kernel_root;

    ipc_init();
    cap_system_init();
    user_syscall_backend_reset();
    cap_endpoint_register_type();
    cap_as_register_type();

    sm_oid = 0;
    pm_oid = 0;
    fs_oid = 0;
    console_oid = 0;
    vm_oid = 0;
    init_ready_oid = 0;

    ep_sm = ep_create();
    ep_pm = ep_create();
    ep_fs = ep_create();
    ep_console = ep_create();
    ep_vm = ep_create();
    ep_init_ready = ep_create();
    if (ep_sm == 0 || ep_pm == 0 || ep_fs == 0 || ep_console == 0 || ep_vm == 0 ||
        ep_init_ready == 0) {
        return -1;
    }

    if (cap_object_create(&cap_endpoint_type, ep_sm, &sm_oid) != CAP_OK ||
        cap_object_create(&cap_endpoint_type, ep_pm, &pm_oid) != CAP_OK ||
        cap_object_create(&cap_endpoint_type, ep_fs, &fs_oid) != CAP_OK ||
        cap_object_create(&cap_endpoint_type, ep_console, &console_oid) != CAP_OK ||
        cap_object_create(&cap_endpoint_type, ep_vm, &vm_oid) != CAP_OK ||
        cap_object_create(&cap_endpoint_type, ep_init_ready, &init_ready_oid) !=
            CAP_OK) {
        return -1;
    }

    if (install_root(SERVICE_SID_SM, SLOT_SM_RECV, sm_oid, CAP_RIGHT_READ, 0,
                     &sm_recv) != 0 ||
        install_root(SERVICE_SID_SM, SLOT_SM_PM, pm_oid,
                     CAP_RIGHT_WRITE | CAP_RIGHT_GRANT, 0, &sm_pm) != 0 ||
        install_root(SERVICE_SID_SM, SLOT_SM_FS, fs_oid,
                     CAP_RIGHT_WRITE | CAP_RIGHT_GRANT, 0, &sm_fs) != 0 ||
        install_root(SERVICE_SID_SM, SLOT_SM_CONSOLE, console_oid,
                     CAP_RIGHT_WRITE | CAP_RIGHT_GRANT, 0, &sm_console) != 0 ||
        install_root(SERVICE_SID_SM, SLOT_SM_VM, vm_oid,
                     CAP_RIGHT_WRITE | CAP_RIGHT_GRANT, 0, &sm_vm) != 0) {
        return -1;
    }

    if (install_root(SERVICE_SID_PM, SLOT_PM_RECV, pm_oid, CAP_RIGHT_READ, 0,
                     &pm_recv) != 0 ||
        install_root(SERVICE_SID_PM, SLOT_PM_SM, sm_oid, CAP_RIGHT_WRITE, 0,
                     &pm_sm) != 0 ||
        install_root(SERVICE_SID_PM, SLOT_PM_VM, vm_oid, CAP_RIGHT_WRITE, 0,
                     &pm_vm) != 0) {
        return -1;
    }

    if (install_root(SERVICE_SID_INIT, SLOT_INIT_SM_RECV, sm_oid, CAP_RIGHT_READ,
                     0, &init_sm_recv) != 0 ||
        install_root(SERVICE_SID_INIT, SLOT_INIT_SM_SEND, sm_oid, CAP_RIGHT_WRITE,
                     0, &init_sm_send) != 0 ||
        install_root(SERVICE_SID_INIT, SLOT_INIT_READY, init_ready_oid,
                     CAP_RIGHT_READ, 0, &init_ready) != 0) {
        return -1;
    }

    if (install_root(SERVICE_SID_VM, SLOT_CHILD_RECV, vm_oid, CAP_RIGHT_READ, 0,
                     &vm_recv) != 0) {
        return -1;
    }
    if (install_root(SERVICE_SID_VM, SLOT_CHILD_SM, sm_oid, CAP_RIGHT_WRITE, 0,
                     &vm_sm) != 0) {
        return -1;
    }
    if (install_root(SERVICE_SID_HEAPTEST, SLOT_HEAP_VM, vm_oid,
                     CAP_RIGHT_WRITE, 0, &heap_vm) != 0 ||
        install_root(SERVICE_SID_HEAPTEST2, SLOT_HEAP_VM, vm_oid,
                     CAP_RIGHT_WRITE, 0, &heap2_vm) != 0) {
        return -1;
    }

    pager_init(ep_vm);

    if (out != 0) {
        out->sm = (struct loader_boot_args){
            .cap_sid = SERVICE_SID_SM,
            .a0 = sm_recv,
            .a1 = sm_pm,
            .a2 = sm_fs,
            .a3 = sm_console,
            .a4 = sm_vm,
        };
        out->pm = (struct loader_boot_args){
            .cap_sid = SERVICE_SID_PM,
            .a0 = pm_recv,
            .a1 = pm_sm,
            .a2 = SERVICE_SID_PM,
            /* 子进程被 discard 后，PM 用它通知 VM 释放该 tid 的 region。 */
            .a3 = pm_vm,
        };
        out->init = (struct loader_boot_args){
            .cap_sid = SERVICE_SID_INIT,
            .a0 = init_sm_recv,
            .a1 = init_sm_send,
            .a2 = init_ready,
            .a3 = 3u,
            .a4 = SERVICE_SID_INIT,
            .a5 = 0,
        };
        out->vm = (struct loader_boot_args){
            .cap_sid = SERVICE_SID_VM,
            .a0 = vm_recv,

        };
        out->heaptest = (struct loader_boot_args){
            .cap_sid = SERVICE_SID_HEAPTEST,
            .a0 = heap_vm,
            .a1 = 0,
        };
        out->heaptest2 = (struct loader_boot_args){
            .cap_sid = SERVICE_SID_HEAPTEST2,
            .a0 = heap2_vm,
            .a1 = 1,
        };
    }
    return 0;
}