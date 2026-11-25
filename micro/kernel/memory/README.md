# memory：物理帧与 Sv39 地址空间

本目录实现微内核最小模型里的「空间层」：物理帧所有权 + VA → Frame 的部分映射。
不做 VMA、COW、swap、按需分页策略；这些属于用户态 VM server。

其他路径的代码没有修改；本目录可以独立编译和自测。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `pfn_t`（物理帧） | `memory.h` | 4 KiB 页的物理页号；帧池由 `frame_init()` 从 BootInfo RAM 建立 |
| 帧位图 + 引用计数 | `frame.c` | `used_bitmap[]` 记录占用，`refcounts[]` 记录引用数；`alloc_hint` 指向上次分配位置 |
| `struct as` | `memory.h` | 一个 Sv39 三级页表树：`root_pfn`、`kernel_start`（共享内核的顶层索引下界）、`active`、`cap_oid` |
| PTE | `as.c` | 页表项；`PTE_RSW_NONOWNED`（bit 8）标记「非拥有映射」，销毁时不得 `frame_unref` |
| `struct walk_result` | `as.c` | 页表遍历结果：叶子 PTE 指针 + 命中的 pfn（`PFN_NONE` 表示空槽） |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `objects` | 注册 `OBJ_AS` | `as_create()` 注册；`as_destroy()` 是析构回调 |
| `capability` | AS 的 cap 对象 | `cap_as_bind()` 为 AS 建 cap 对象并记 `as->cap_oid`；`as_destroy()` 通过弱钩子 `cap_as_release()` 反向丢弃它 |
| `loader` | 最主要的 AS 使用方 | `as_create` / `as_share_kernel` / `as_map`（装载 PT_LOAD）/ `as_map_kernel_range`（设备 MMIO）/ `as_activate` |
| `boot/kernel_init` | 建内核 AS | `as_map_kernel_range` 建立内核镜像与帧池的**非拥有**恒等映射，再 `as_share_kernel` 给每个用户 AS |
| `sched` | AS 生命周期 | `thread_create` 对 `as` 做 `as_ref`；`as_activate()` 在上下文切换时写 `satp`；`reap_dead`/`sched_thread_discard` 在安全点 `as_unref` |
| `pager` | 缺页处理 | `as_fault` / `as_translate`；缺页后由 VM 服务经 `ABI_VM_MAP` 调 `as_map` 补页 |
| `syscall/user_backend` | 跨 AS 拷贝 | `ABI_CONSOLE_*` / `ABI_BLOCK_READ` 用 `as_copyin` / `as_copyout` 访问调用者缓冲区 |
| `ipc` | 无直接耦合 | IPC 不映射内存；消息体在 TCB 里 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 纯位图分配器 | 只有 `frame_alloc` / `frame_free`，无引用计数 | 单模块 demo：分配、耗尽、保留区 |
| 2. 引入引用计数 | `frame_ref` / `frame_unref`，`as_map` 加一次、`as_unmap` 减一次 | 同一物理帧要被多个 AS 共享（`as_share_kernel`），单靠 free 无法判断何时真回收 |
| 3. 非拥有映射 | `as_map_kernel_range` 在 PTE 的 RSW bit 8 打标记，`free_table` 与 `as_unmap` 跳过 `frame_unref` | 内核镜像与帧池不在帧池的引用体系里；误回收会让整个系统崩掉 |
| 4. 内核上半区共享 | `as_share_kernel(as, root, first_index)` 复制内核顶层表项 | 用户 AS 在 satp 切换后仍需进入 S 态 trap handler、访问内核代码与帧池 |
| 5. `as_fault` + W^X | 拒绝 `W|X` 组合、未知权限位；用户拷贝额外要求 `PTE_U` | `ABI_VM_MAP` 必须能表达只读页，且不能造出可写可执行的用户页 |
| 6. 页表页回收（A13） | `as_unmap` 解除映射后**自底向上**检查中间页表是否变空，空了就断链 + `frame_unref` | 旧实现只清叶子 PTE，反复 map/unmap 不同 VA 会永久吃掉页表页；demand paging 长跑必然耗尽帧池 |
| 7. 位图字节跳扫 | `frame_find_free` 先按 `0xff` 跳过整字节 | 大池上逐位线性扫描太慢 |

## 最小模型

```text
Frame = (pfn, state ∈ {FREE, ALLOCATED}, refcount)
AS    = (root_pfn, M: VA ⇀ (Frame, rights))

FREE --frame_alloc--> ALLOCATED --as_map--> MAPPED --as_unmap--> ALLOCATED
                                                                  |
                                                          frame_unref(0)
                                                                  v
                                                                 FREE
```

- `Frame` 是物理页，单位固定 4 KiB。
- `AddressSpace` 是 Sv39 三级页表；`M` 是部分函数，未映射的 VA 查询失败。
- `as_map()` 增加一次 frame 引用；`as_unmap()` 减少一次。映射不等于所有权转移。
- 页表页本身也是 frame；`as` 销毁时递归释放所有层级。

## 不变量

1. `FREE` 集合与位图/空闲计数完全一致。
2. 每个有效 PTE 指向 `refcount >= 1` 的 frame。
3. `as_destroy` 递归释放页表页和映射帧；`frame_unref` 到 0 才真正回收。
4. 同一 VA 不允许重复映射；重复 `as_map` 返回 `MEM_EEXIST`。
5. `as_map` 失败时回滚已分配的中间页表页，并清掉父 PTE。
6. 不支持大页；遇到非叶子层的叶子描述符返回 `MEM_EINVAL`。
7. 修改 PTE 后调用 `sfence.vma`；切换 `satp` 后同样刷新 TLB。
8. `as_copyin/as_copyout` 先按页校验权限，再拷贝；跨页/非对齐地址可用。
9. 用户拷贝额外要求 `PTE_U`；不带 `U` 的内核映射不能被 `copyin/copyout` 访问。
10. `as_share_kernel` 共享的顶层表项不参与用户 AS 的引用计数，`as_destroy`
    只回收 `[0, kernel_start)` 的用户部分。
11. `as_share_kernel` 要求目标顶层区间 `[first_index, 512)` 尚未映射；否则返回
    `MEM_EEXIST`，避免覆盖已有 PTE 并泄漏其 frame。
12. `as_map` 只接受 `MEM_R/W/X/U/KERNEL` 这些权限位，未定义的位一律
    `MEM_EINVAL`；至少需要 `R/W/X` 之一，且 `W` 与 `X` 不能同时出现（W^X）。

## Sv39 页表

- 三级页表，每级 512 项，PTE 8 字节，页 4 KiB。
- VPN：`vpn[0] = va[20:12]`，`vpn[1] = va[29:21]`，`vpn[2] = va[38:30]`。
- PTE 位：`V/R/W/X/U/G/A/D`；PPN 从 bit 10 开始。
- `as_map` 总是设置 `A`；用户映射设置 `U`，`MEM_KERNEL` 映射不设置 `U`；
  写权限自动补 `R` 和 `D`。
- `as_share_kernel` 应在映射用户页之前调用；它只允许安装一次。- `as_fault` 的 `access` 如果包含 `MEM_U`，共享内核映射会返回 `MEM_EPERM`。
- `as_activate()` 写 `satp = (8 << 60) | root_pfn`，ASID 固定 0。

## 代码

| 文件 | 内容 |
|---|---|
| `memory.h` | Frame/AS API、权限与错误码、PTE 约定 |
| `frame.c` | 位图 + 引用计数帧分配器、保留区、耗尽检测 |
| `as.c` | Sv39 页表遍历、map/unmap/translate/fault、copyin/out、AS 销毁 |
| `memory_test.c` | 映射/翻译/缺页/跨页拷贝/AS 回收/帧耗尽自测 |
| `Makefile` | 主机自测构建 |

## API

```c
int  frame_init(paddr_t phys_base, size_t size, void *direct_map);
void frame_reserve(paddr_t pa, size_t size);
pfn_t frame_alloc(void);
int  frame_free(pfn_t pfn);
void frame_ref(pfn_t pfn);
void frame_unref(pfn_t pfn);
uint32_t frame_refcount(pfn_t pfn);
uint64_t frame_free_count(void);
uint64_t frame_total_count(void);
void *frame_ptr(pfn_t pfn);

struct as *as_create(void);
void as_ref(struct as *as);
void as_unref(struct as *as);

int as_map(struct as *as, vaddr_t va, pfn_t pfn, int rights);
int as_unmap(struct as *as, vaddr_t va);
int as_translate(struct as *as, vaddr_t va, int *rights, pfn_t *pfn);
int as_fault(struct as *as, vaddr_t va, int access, pfn_t *pfn);
int as_copyin(struct as *as, vaddr_t uva, void *dst, size_t len);
int as_copyout(struct as *as, vaddr_t uva, const void *src, size_t len);
int as_share_kernel(struct as *as, pfn_t kernel_root, int first_index);
int as_map_kernel_range(struct as *as, paddr_t base, size_t size);
void as_activate(struct as *as);
```

所有权约定：

```c
pfn_t p = frame_alloc();          /* p 的 refcount = 1，调用者持有 */
as_map(as, va, p, MEM_R | MEM_W); /* refcount = 2：调用者 + 映射 */
frame_unref(p);                   /* 调用者交出所有权，refcount = 1：只剩映射 */
```

如果 `as_map` 失败，调用者仍持有自己的那一份引用，需要自行 `frame_unref`。

## 构建与自测

```sh
make -C micro/kernel/memory test
```

预期输出：

```text
memory: map/copy/unmap ok
memory: kernel share ok
memory: non-owned kernel map ok
memory: page table reclaim ok
memory: reserve/exhaust ok
memory: all tests passed
```

`page table reclaim` 覆盖 `as_unmap` 的页表回收：解除映射后，自底向上把
变空的中间页表页断链并 `frame_unref`。旧实现只清叶子 PTE，反复
map/unmap 不同 VA 会永久吃掉页表页，demand paging 长跑必然耗尽帧池。

## QEMU 接线

```sh
make -C micro run BOOT_MODULES=memory
```

自举 demo 使用一段对齐的 RAM 建立 Sv39 页表，验证翻译、用户拷贝、撤销映射及帧回收。

```c
frame_init(0x80000000, 128 * 1024 * 1024, direct_map_ptr);
frame_reserve((paddr_t)kernel_start, kernel_size);
struct as *as = as_create();
as_share_kernel(as, kernel_as->root_pfn, KERNEL_VPN2);
as_map(as, USER_VA, frame, MEM_R | MEM_W);
as_activate(as); /* 写 satp + sfence.vma */
```

`direct_map_ptr` 是物理内存的内核直接映射起点；`frame_ptr(pfn)` 返回该帧的内核
可访问地址。QEMU virt 默认 RAM 从 `0x80000000` 开始，UART 在 `0x10000000`，
内核需要在启动时单独映射 UART。

内核页表通常是永久对象。创建用户 AS 后，用下面这种方式把内核顶层区间共享进去：

```c
struct as *kernel_as = ...; /* 启动时建立，永不销毁 */
struct as *user_as = as_create();
as_share_kernel(user_as, kernel_as->root_pfn, KERNEL_VPN2);
```

`KERNEL_VPN2` 是内核虚拟地址对应的 Sv39 顶层索引（例如 `0x80000000` 对应 2）。
共享表项不会被用户 AS 释放；内核 AS 必须比所有用户 AS 活得久。

### 本次接线：`as_map_kernel_range()`

`as_map()` 要求目标物理帧的 `refcount >= 1`，这只适合 `frame_alloc()` 的对象。
这里有两类物理页：

- **内核镜像**由链接脚本/固件提供，不是 `frame_init` 的对象；
- **直接映射窗口**是 `frame_init` 管理的帧池，池中的帧由 `frame_alloc()` 及其
  持有者管理引用。

两者对这个内核 AS 都是**非拥有映射**：`as_map_kernel_range(as, base, size)` 做
逐页恒等映射（VA == PA、`PTE_U=0`），不增加 refcount，并在 PTE 的 RSW bit 8
写“非拥有”标记。`as_unmap()` 和 AS 销毁遍历 `free_table()` 时看到该标记会跳过
`frame_unref()`，所以销毁带恒等映射的 AS 不会误回收内核镜像或帧池 owner 仍持有
的页。它只在 satp 第一次启用前调用，用来建立“所有用户 AS 共享的内核根页表”。

```c
struct as *kernel_as = as_create();
as_map_kernel_range(kernel_as, (paddr_t)__kernel_start, kernel_image_size);
as_map_kernel_range(kernel_as, (paddr_t)pool_base, pool_size);

struct as *user_as = as_create();
as_share_kernel(user_as, kernel_as->root_pfn, 2); /* VA >= 0x80000000 */
```

`kernel/syscall/closure.c` 用这条路径让两个用户 AS 在 satp 切换后仍能进入
S 态 trap handler、访问 frame 池中的页表页，并在日志里留下不同的 root pfn。
主机自测 `memory_test.c` 覆盖“销毁带非拥有恒等映射的 AS 不改变池帧 refcount”。
完整运行记录见 [`docs/evidence/qemu-unified-legacy.log`](../../docs/evidence/qemu-unified-legacy.log)。

## 已知边界

- 无大页、无 ASID、无 SMP TLB shootdown。
- `as_copyin/out` 的「先校验再拷贝」在单核上成立；SMP 下需要锁或 pin。
- **默认 capability 镜像**：U-mode 缺页经 `pager` → `TAG_VM_FAULT` → U-mode `vm`（`ABI_VM_MAP`）；单模块 demo 仍可直接 `MEM_EFAULT`。
- 不做 COW、swap、VMA、反向映射。
- 页表页回收只处理「变空」的情形（A13）。若某个中间页表仍有一个有效项，它就不会被回收——这是正确行为，但也意味着稀疏映射（每个页表页只放一页）会占更多内存。`AS_MAX` 与帧池容量是硬上限。
