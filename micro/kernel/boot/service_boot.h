#ifndef MICRO_KERNEL_SERVICE_BOOT_H
#define MICRO_KERNEL_SERVICE_BOOT_H

#include "bootinfo.h"
#include "capability.h"
#include "loader/loader.h"
#include "memory.h"

struct thread;

enum service_sid {
    SERVICE_SID_INIT = 1,
    SERVICE_SID_SM = 2,
    SERVICE_SID_PM = 3,
    SERVICE_SID_FS = 4,
    SERVICE_SID_CONSOLE = 5,
    SERVICE_SID_SHELL = 6,
    SERVICE_SID_VM = 7,
    SERVICE_SID_HEAPTEST = 8,
    SERVICE_SID_HEAPTEST2 = 9,
    SERVICE_SID_MAX = 10,
};

/*
 * 内核：sm/pm/vm/heaptest/heaptest2/init；init 经 PM 拉 fs/console/shell（plan=3）。
 */
struct service_boot_layout {
    struct loader_boot_args init;
    struct loader_boot_args sm;
    struct loader_boot_args pm;
    struct loader_boot_args vm;
    struct loader_boot_args heaptest;
    struct loader_boot_args heaptest2;
};

int service_boot_prepare(const struct bootinfo *bi, pfn_t kernel_root,
                         struct service_boot_layout *out);

/* PM 拉起的子进程向 init 回执 ready 的发送能力（每个 SID 的 slot 1）。 */
int service_stub_ready_send(cap_sid_t sid, uint64_t receipt, cap_handle_t *send_out);

/* 子服务访问 SM 名称服务的发送能力（每个 SID 的 slot 3）。 */
int service_sm_install_send(cap_sid_t sid, cap_handle_t *send_out);

/* FS / console / VM 服务自己的 recv 能力（每个 SID 的 slot 2）。 */
int service_fs_install_service_recv(cap_handle_t *recv_out);
int service_console_install_service_recv(cap_handle_t *recv_out);
int service_vm_install_service_recv(cap_handle_t *recv_out);

/* PM discard：拆除子服务 CSpace 上 spawn 时装的固定槽（1=ready,3=sm；可选 2=recv）。 */
void service_teardown_child_caps(cap_sid_t sid, bool has_service_recv);

#endif