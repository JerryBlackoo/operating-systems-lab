# vm 用户态虚拟内存策略

VM 服务记录地址空间中允许访问的区间，并在页首次访问时分配物理帧。模型从合成缺页事件出发，按需分配并清零匿名页；帧分配、清零和页面映射通过 `vm_port` 交给 [内核内存模型](../../../kernel/memory/README.md)。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct vm_region` | `vm.h` | 一个匿名区间：`as`（地址空间 id）、`lo`/`hi`、`rights`（`ABI_READ/WRITE/EXEC`） |
| `struct vm_page` | `vm.h` | 一条驻留记录：`as`、`va`（页对齐）、`frame`（物理帧号） |
| `struct vm` | `vm.h` | 服务状态：`port`、`regions[VM_REGIONS=4]`、`pages[VM_PAGES=8]` |
| `struct vm_port` | `vm.h` | **唯一的硬件缝**：`alloc` / `clear` / `map` / `unmap` / `unref` |
| `enum vm_result` | `vm.h` | `VM_OK` / `VM_RESUME` / `VM_DENIED` / `VM_CONFLICT` / `VM_NO_SPACE` / `VM_MAP_ERROR` 等 |
| resident 的 `struct vm_user_ctx` | `user/vm_main.c` | `vm_port` 的 U-mode 实现上下文；`u_map` 里调 `ABI_VM_MAP` |
| `#define VM_PAGE` / `VM_LIMIT` | `vm.h` | 页大小 4096 与虚拟地址上限 |

**容量是硬上限**：4 个区间、8 个驻留页。超出分别返回 `VM_NO_SPACE`，
两者是**不同**的耗尽原因（前者是区间表，后者是驻留表）。

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `kernel/pager` | 缺页请求来源 | 内核投递 `TAG_VM_FAULT`（含 tid/va/access），VM 处理后回 `ABI_PAGER_DONE` 或 `ABI_PAGER_FAIL` |
| `kernel/syscall/user_backend` | 补页落地 | `ABI_VM_MAP`：校验 `pager_authorize_map(tid, va, as_id)` 票 **且** `cap_vm_can_map_as`；内核 `frame_alloc` + 清零 + `as_map` |
| `capability`（`cap_as.c`） | AS 的 MAP 权 | `cap_vm_can_map_as` 要求 VM 的 CSpace 持目标 AS 的 `CAP_RIGHT_MAP`；由 PM 的 `ABI_VM_GRANT_AS` 授予 |
| `pm` | 进程退出回收 | `TAG_VM_FORGET`（只接受 `USER_SID_PM`）→ `vm_forget(as)` 一次丢掉该 AS 的全部区间与驻留页 |
| `kernel/memory` | 单模块 demo 的端口实现 | `vm` host demo 把 `vm_port` 直接绑到真实的 `frame_alloc`/`as_map`/`as_unmap` |
| `heaptest` / `heaptest2` | 客户端 | 用 `TAG_VM_RESERVE` 登记区间，然后访问未映射页触发真实缺页 |
| `sm` | 服务发现 | VM 启动后 `TAG_SM_REGISTER` 注册 `"vm"`（实验路径）；默认链由内核直 spawn，heaptest 用预置 cap |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 纯模型（内存替身） | `fake_memory`：8 个假帧 + 映射表，验证区间/权限/驻留复用/失败回滚 | 先把「区间是权限与地址的声明」这个语义定下来，不碰页表 |
| 2. 绑定真实内核端口 | demo 第二套用例把 `vm_port` 接到 `frame_alloc`/`clear`/`as_map`/`as_unmap` | 验证 `alloc → clear → map → unref` 的引用交接：成功路径映射引用恰为 1，失败路径帧被回收 |
| 3. `VM_RESUME` 语义 | 命中已驻留页直接返回 `VM_RESUME`，不重复分配 | 同一页的多次访问（读写、跨页偏移）必须复用同一帧 |
| 4. 区分两种耗尽 | `VM_NO_SPACE`（区间表满）与 `VM_NO_SPACE`（驻留表满）在 demo 里分别断言 | 两者的诊断与扩容方向完全不同 |
| 5. U-mode VM 服务（M6） | `vm_main.c`：`TAG_VM_RESERVE` + `TAG_VM_FAULT`；`u_map` 调 `ABI_VM_MAP` | 缺页策略必须离开内核；内核只保留 pager 与 `ABI_VM_MAP` 的权限校验 |
| 6. 授权票 | `pager_authorize_map(tid, va, as_id)`：一次性票，绑定三元组 | 否则 VM 服务能映射任意进程的任意 VA，等于把整个地址空间交给它 |
| 7. 要求 VM 持 AS 的 MAP cap（A2） | `cap_vm_can_map_as` + PM 的 `ABI_VM_GRANT_AS` 减权 mint | 票只证明「这次缺页是真的」，不证明「VM 有权改这个 AS」 |
| 8. `TAG_VM_RELEASE`（A14） | `vm_release` 逐页 unmap 并归还区间/驻留槽 | `vm_release` 早已实现且有 host 用例，但**协议里没有释放消息**，resident 从不调用它——每次 reserve 永久吃一个槽 |
| 9. `TAG_VM_FORGET` + 身份校验（A17/A19） | `vm_forget(as)` 一次清理该 AS；VM 用 `ABI_REPLY_CALLER_SID` 校验发送者是 PM | 崩溃或被杀掉的进程不会自己 release；而 heaptest 也持 VM 发送权，不校验就能清掉别人的区间 |

## 最小模型

```text
Region   = (address_space, [lo, hi), rights)
VM       = (regions, resident)
resident : (address_space, virtual_page) ⇀ frame

无区间 ──reserve──> 已预留 ──合法缺页──> 已预留且相应页已映射
                        └──release───────────────> 无区间
```

| 操作 | 迁移 |
| --- | --- |
| `reserve(as, lo, len, rights)` | 检查非空区间的页对齐、范围和权限，确认终址计算无溢出、区间无冲突后登记 |
| `fault(as, va, access)` | 查找区间并检查访问权限。有驻留页时复用，否则分配一帧、清零并映射，完成后返回 `VM_RESUME` |
| `release(as, lo, len)` | 对完整匹配的区间逐页撤销驻留映射，再移除区间 |

缺页映射成功后，页表持有映射引用，服务释放临时分配引用。映射失败时释放临时帧，该页仍处于已预留状态。模型中的内存端口保证可撤销它创建的有效映射。

## 不变量

1. 同一地址空间内的区间互不重叠，区间之间可以邻接。不同地址空间各自管理虚拟页。
2. 驻留页落在所属区间内，每个虚拟页最多一条映射，权限受区间限制。
3. 只有区间内且访问权限成立的缺页可以建立映射。新建匿名页对用户可见前已清零。
4. 释放区间同时移除其驻留页记录和映射引用。
5. `VM_PAGE`、`VM_REGIONS`、`VM_PAGES` 只是教学模型的容量上限；容量耗尽分别返回 `VM_NO_SPACE`。

## 内存端口

```c
struct vm_port {
    void *ctx;
    uint32_t (*alloc)(void *);
    void (*clear)(void *, uint32_t frame);
    bool (*map)(void *, uint32_t as, uint64_t va, uint32_t frame, unsigned rights);
    void (*unmap)(void *, uint32_t as, uint64_t va);
    void (*unref)(void *, uint32_t frame);
};
```

`alloc` 返回的帧句柄带有一份调用者引用。`map` 成功时页表必须再持有一份引用；`vm_fault` 统一释放分配时的临时引用，因此成功路径上映射引用恰好为 1，失败路径上帧被回收。`unmap` 释放映射引用。

`vm.c` 只依赖这六个函数，不包含任何 Sv39 页表遍历或物理分配器代码。

## 独立验证

- 区间：拒绝未对齐、零长、越界、写不带读、未知权限位和同 AS 重叠；允许邻接区间与不同 AS 的同址区间。
- 缺页：连续访问同一页只分配一帧；越界、写只读页、非法 access 被拒绝；`map` 失败后临时帧归零；帧耗尽与驻留表耗尽分别返回 `VM_NO_SPACE`。
- 释放：只接受完整匹配的区间；逐页撤销映射并移除该区间，其他 AS 的驻留页不受影响。

## kernel/memory 接线

`demo.c` 的第二部分把 `vm_port` 真正绑定到 `kernel/memory`：

```c
alloc -> frame_alloc
clear -> frame_ptr + memset
map   -> as_map(as, va, pfn, MEM_U | rights)
unmap -> as_unmap(as, va)
unref -> frame_unref
```

验证内容包括：ABI 权限到 PTE 权限的转换、映射后帧引用为 1、同一虚拟页的写访问复用驻留页、映射失败后帧计数不变，以及释放后 `as_translate` 返回 `MEM_EFAULT` 且帧数回到基线。模型本身仍不拥有帧或页表；这部分只在 demo 的接线层出现。

## QEMU demo

```sh
# 独立模型：内存替身 + kernel/memory 接线
make -C micro/userland/servers/vm run

# 只把 vm 编进内核镜像
make -C micro run BOOT_MODULES=vm

# 全部模块的完整镜像
make -C micro run BOOT_MODULES=all
```

从仓库根目录运行。成功时输出 `[vm] PASS` 并退出 QEMU。demo 在 S-mode 运行，因为接线部分要执行 Sv39 页表操作；正式系统中该端口位于内核，VM 服务通过 IPC 访问它。

## capability 路径（M6）

| 项 | 状态 |
| --- | --- |
| U-mode `vm.elf` | 内核 spawn；`TAG_VM_RESERVE` / `TAG_VM_RELEASE` / `TAG_VM_FORGET` + `TAG_VM_FAULT`（内核 pager 投递） |
| 补页 | `ABI_VM_MAP` / `ABI_PAGER_DONE`（`pager_authorize_map` 票） |
| 测试 | `heaptest` / `heaptest2`；`tools/m6_discriminate.sh` |
| 错误 | 未知/畸形请求回 `TAG_VM_ERROR`；`TAG_VM_FAULT` 只接受内核 POST（`reply=0`） |

已做：内核侧 fault 排队（`ep_post` + `IPC_MAX_POSTS`，VM 不在 recv 时不丢
缺页请求）；**`TAG_VM_RELEASE`（2026-11-25）** 让客户端显式撤销区间，
resident 走 `vm_release` 逐页 unmap 并归还 `VM_REGIONS` / `VM_PAGES` 槽位；
**`TAG_VM_FORGET`（2026-11-25）** 由 PM 在 `ABI_PM_DISCARD` 成功后发出
（用 `call` 而非 `send`，因为 VM 需要 reply token 才能查发送者身份），
VM 用 `vm_forget(as)` 一次丢掉该地址空间的全部 region 与驻留页。
**VM 只接受 `USER_SID_PM`**：heaptest/heaptest2 也持 VM 发送能力（M6 需要），
不校验时它们能清掉任意进程的区间。

**验证状态（勿过声称）**：`vm_forget` 本身由 host demo 的
`forget drops one address space without touching others` 覆盖（含跨 AS
隔离与帧引用归零）。但**端到端路径未验证**——默认启动链没有进程被
discard，所以 `TAG_VM_FORGET` 在 QEMU 里不会触发。身份校验是纯比较逻辑，
风险低；要做端到端验证需要构造一次 spawn 失败（例如填满 PM 的 16 槽进程表）。
**pager 缺页超时（2026-11-25）**：`PAGER_TIMEOUT_NS` 到期唤醒缺页线程
并返回 `ABI_TIMED_OUT`，不再无限阻塞。
未做：init 经 PM 拉 VM（默认链由内核 spawn；`kernel_pm_spawn`
的 SPAWN_VM 分支可用但未纳入验收）。
VM 的 region/page 表固定 4/8 项。`vm` host demo 的
`region and resident slots are reused across many cycles` 与
`forget drops one address space without touching others` 覆盖槽位复用与
按 AS 清理。
