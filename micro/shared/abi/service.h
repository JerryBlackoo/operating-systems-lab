#ifndef MICRO_ABI_SERVICE_H
#define MICRO_ABI_SERVICE_H

#include <stdint.h>

/*
 * 服务 SID（主体身份）。
 *
 * 内核与用户态共用同一份定义：内核用它做权限校验（`request->caller`），
 * 用户态用它填写 SM 注册/查询里的 sid。两份不同的副本会让「谁是谁」
 * 在两侧漂移，所以这里只定义一次，`userland/libs/service_proto.h` 引用本头。
 *
 * 这些值必须与 `kernel/boot/service_boot.c` 的 `install_root(...)` 调用
 * 以及各服务的 `image_sid()` 保持一致。
 */
enum service_sid_user {
    USER_SID_INIT = 1,
    USER_SID_SM = 2,
    USER_SID_PM = 3,
    USER_SID_FS = 4,
    USER_SID_CONSOLE = 5,
    USER_SID_SHELL = 6,
    USER_SID_VM = 7,
    USER_SID_HEAPTEST = 8,
    USER_SID_HEAPTEST2 = 9,
    USER_SID_MAX = 10,
};

/* 与 userland/libs/service_proto.h 对齐的 IPC tag（内核与用户态共用）。 */
enum ipc_service_tag {
    TAG_SM_REGISTER = 0x201,
    TAG_SM_LOOKUP = 0x202,
    TAG_SM_OK = 0x203,
    TAG_SM_ERROR = 0x204,

    TAG_PM_SPAWN = 0x301,
    TAG_PM_SPAWN_OK = 0x302,
    TAG_PM_PS = 0x311,
    TAG_PM_PS_OK = 0x312,
    TAG_PM_ERROR = 0x313,

    TAG_READY = 0x401,

    TAG_FS_PATH = 0x501,
    TAG_FS_PATH_OK = 0x502,
    TAG_FS_READ = 0x503,
    TAG_FS_READ_OK = 0x504,
    TAG_FS_ERROR = 0x505,

    TAG_CONSOLE_WRITE = 0x601,
    TAG_CONSOLE_WRITE_OK = 0x602,
    TAG_CONSOLE_READ = 0x603,
    TAG_CONSOLE_READ_OK = 0x604,
    TAG_CONSOLE_MORE = 0x605,
    TAG_CONSOLE_MORE_OK = 0x606,
    TAG_CONSOLE_ERROR = 0x607,

    /* M6：内核向 VM 投递缺页；words[0]=fault_tid words[1]=va words[2]=access。 */
    TAG_VM_FAULT = 0x701,
    /* M6：进程向 VM 显式登记 region；words[0]=tid words[1]=va
     * words[2]=len words[3]=rights。应答 TAG_VM_RESERVE_OK。 */
    TAG_VM_RESERVE = 0x702,
    TAG_VM_RESERVE_OK = 0x703,
    /* M6：heaptest 完成 reserve/map 判别后通知 init，可继续拉起 shell。 */
    TAG_VM_PHASE_DONE = 0x704,
    /* 未知请求/坏参数；仅带 reply 的调用会收到。 */
    TAG_VM_ERROR = 0x705,
    /*
     * 撤销一个 region：words[0]=tid words[1]=va words[2]=len。
     * 应答 TAG_VM_RELEASE_OK。不提供这条路径时 VM 的 region/驻留页槽位
     * 只增不减，反复 reserve/release 的进程会永久耗尽 VM_REGIONS。
     */
    TAG_VM_RELEASE = 0x706,
    TAG_VM_RELEASE_OK = 0x707,
    /*
     * 进程退出通知：words[0]=tid。由 PM 在 ABI_PM_DISCARD 成功后 fire-and-forget
     * 发出，VM 据此释放该地址空间的全部 region 与驻留页。
     *
     * 为什么需要：VM 的 region/驻留页表都是固定大小，而它看不到进程生命周期。
     * 没有这条消息，任何崩溃或被杀掉的进程都会把自己用过的槽位永久占住。
     */
    TAG_VM_FORGET = 0x708,
};

#endif