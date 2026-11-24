# vm 用户态虚拟内存策略

VM 服务记录地址空间中允许访问的区间，并在页首次访问时分配物理帧。模型从合成缺页事件出发，按需分配并清零匿名页；帧分配、清零和页面映射通过 `vm_port` 交给 [内核内存模型](../../../kernel/memory/README.md)。

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
