#ifndef MICRO_MODEL_VM_H
#define MICRO_MODEL_VM_H

#include <stdbool.h>
#include <stdint.h>

#include "abi/abi.h"

/*
 * 用户态虚拟内存策略。
 *
 * VM 只保存两样东西：
 *   Region   = (address_space, [lo, hi), rights)
 *   resident : (address_space, virtual_page) -> frame
 *
 * 帧分配、清零和页表映射不属于本模块；它们通过 vm_port 由
 * kernel/memory 提供。VM 不包含任何物理分配器或页表遍历代码。
 */

enum { VM_PAGE = 4096, VM_REGIONS = 4, VM_PAGES = 8 };

/* Sv39 用户地址上限（不含）；与 kernel/memory 的地址空间检查保持一致。 */
#define VM_LIMIT (UINT64_C(1) << 38)

enum vm_result {
    VM_OK = 0,
    VM_BAD_RANGE,
    VM_CONFLICT,
    VM_DENIED,
    VM_NO_SPACE,
    VM_MAP_ERROR,
    VM_RESUME, /* vm_fault 已处理缺页，可以恢复原指令 */
};

/*
 * 内核内存端口。
 *
 *   alloc() 返回帧句柄，0 表示失败；
 *   map() 成功时必须持有一份帧引用，vm_fault 随后释放临时分配引用；
 *   unmap() 撤销映射并释放映射持有的那份引用；
 *   unref() 释放调用者持有的引用。
 *
 * 这个结构是 VM 与 kernel/memory 之间唯一的缝：模型里可以直接用函数
 * 指针绑定，正式系统里换成 IPC 请求/应答不影响上层策略。
 */
struct vm_port {
    void *ctx;
    uint32_t (*alloc)(void *);
    void (*clear)(void *, uint32_t frame);
    bool (*map)(void *, uint32_t as, uint64_t va, uint32_t frame, unsigned rights);
    void (*unmap)(void *, uint32_t as, uint64_t va);
    void (*unref)(void *, uint32_t frame);
};

struct vm_region { uint32_t as; uint64_t lo, hi; unsigned rights; };
struct vm_page { uint32_t as; uint64_t va; uint32_t frame; };

struct vm {
    const struct vm_port *port;
    struct vm_region regions[VM_REGIONS];
    struct vm_page pages[VM_PAGES];
};

/*
 * 登记一个匿名区间。lo 与 len 都必须页对齐，len 非零，且区间不越过
 * VM_LIMIT。rights 只能是 ABI_READ/ABI_WRITE/ABI_EXEC 的非空组合，
 * 且 ABI_WRITE 必须同时带 ABI_READ。同一地址空间的区间不得重叠。
 */
enum vm_result vm_reserve(struct vm *, uint32_t as, uint64_t lo, uint64_t len, unsigned rights);

/*
 * 处理一次缺页。va 可以在页内任意偏移；access 必须是单个权限位。
 * 命中已驻留页时直接返回 VM_RESUME；否则分配、清零并映射一页。
 * 映射失败时释放临时帧，区间保持不变。
 */
enum vm_result vm_fault(struct vm *, uint32_t as, uint64_t va, unsigned access);

/*
 * 撤销一个完整匹配的区间：逐页撤销映射，删除驻留页记录，再释放区间槽位。
 */
enum vm_result vm_release(struct vm *, uint32_t as, uint64_t lo, uint64_t len);

#endif
