# memory：物理帧与 Sv39 地址空间

本目录实现微内核最小模型里的“空间层”：物理帧所有权 + VA → Frame 的部分映射。
不做 VMA、COW、swap、按需分页策略；这些属于用户态 VM server。

其他路径的代码没有修改；本目录可以独立编译和自测。

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
    `MEM_EINVAL`；至少需要 `R/W/X` 之一。

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
memory: reserve/exhaust ok
memory: all tests passed
```

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
as_map(as, USER_VA, frame, MEM_R | MEM_W | MEM_X);
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
完整运行记录见 [`kernel/syscall/qemu-unified.log`](../syscall/qemu-unified.log)。

## 已知边界

- 无大页、无 ASID、无 SMP TLB shootdown。
- `as_unmap` 不回收变空的中间页表页，统一在 AS 销毁时回收。
- `as_copyin/out` 的“先校验再拷贝”在单核上成立；SMP 下需要锁或 pin。
- 无 pager 时缺页返回 `MEM_EFAULT`；接用户态 VM server 时把它转成 IPC 即可。
- 不做 COW、swap、VMA、反向映射。
