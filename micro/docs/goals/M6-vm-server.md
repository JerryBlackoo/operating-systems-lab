# [M6] VM 服务接管虚拟内存

**依赖**：M3  
**建议 Agent**：A6  
**波次**：5

## 背景

`kernel/memory/README.md` 第 184 行明确写了归属：

> 无 pager 时缺页返回 `MEM_EFAULT`；接用户态 VM server 时把它转成 IPC 即可。
> 不做 COW、swap、VMA、反向映射。

而 `userland/servers/vm/` 现在是**纯模型**：`DEMO_MODULE(vm, false, run, NULL)`——连 U-mode 都不是，`vm_port` 的实现（`demo.c:304`）用的是 `static kernel_frames[]` 编译期数组。

所以"微内核把内存策略放到用户态"这句话，目前只有模型没有实例。

## 教材对照

| 王道 | 汤小丹 |
| --- | --- |
| 3.1 内存管理基础（分页、地址变换） | 第 6 章 存储器管理 |
| 3.2 虚拟内存管理（请求分页、缺页） | 第 7 章 虚拟内存 |

## 目标

1. **vm 在 U-mode 且有自己的 AS**：`DEMO_MODULE(vm, true, ...)`，由 M2 的加载器从 initrd 载入，由 M3 的 init/PM 拉起。
2. **region 管理搬进 vm 服务**：`reserve / release / fault` 三个操作的真实实现替代 `vm_port` 的 demo 实现。内核只保留原语：
   - `as_map` / `as_unmap` / `as_map_kernel_range`（已有）
   - **`as_fault` 改为"报告 + 阻塞"**：缺页时内核不分配，而是把 `(as, va, access)` 通过 IPC 发给 vm 服务，线程阻塞；vm 服务调 `as_map` 后回复，线程恢复。
3. **内核侧新增 syscall**（或扩展现有 `ABI_AS_MAP`）供 vm 服务使用：`ABI_AS_MAP` 已经存在（`shared/abi/abi.h` 的 `ABI_AS_MAP = 7`）。需要补的是"代表**另一个** AS 做映射"的权限检查——vm 服务必须持有目标 AS 的 cap，否则就是任意进程都能改别人页表。这条检查必须写进 `kernel/capability/README.md`。
4. **首次访问 heap 页成功**：用户程序 `sbrk` 风格地访问一个未映射页，由 vm 服务补页，程序继续执行。日志要能区分"内核直接映射"和"vm 服务补页"两条路径。
5. **失败语义**：vm 服务不可用 / 拒绝 / 容量耗尽时，缺页必须变成明确的进程错误（`MEM_EFAULT` → 进程退出或 `ABI_NO_MEMORY`），不许静默重试导致活锁。

## 交付

- `userland/servers/vm/`（真实端口 + `user=true`）
- `kernel/memory/as.c`（`as_fault` 的 pager 钩子；保持"无 pager 时返回 `MEM_EFAULT`"的默认行为，好让现有 host 测试不变）
- `kernel/syscall/`（`fault → IPC` 的接缝，或新的 pager 协议）
- `shared/abi/abi.h`（若新增 pager 消息形状）+ `userland/libs/runtime.c`
- `kernel/memory/README.md` 的「已知边界」更新（"无 pager"那条要改成"默认无 pager，M6 起由 vm 服务提供"）
- `docs/design/M6-vm-server.md`、`docs/evidence/M6-<日期>.log`

## 验收

```bash
timeout 25s make -C micro run; echo "EXIT=$?"
# 期望：
#   [vm] serving in U-mode as=<root_pfn>
#   [vm] reserve va=0x... len=... ok
#   [fault] va=0x... access=R -> pager request
#   [vm] map va=0x... frame=0x... ok
#   [fault] resumed
# 且 EXIT=124
```

判别性验收（不许只看"跑通"）：

```bash
# 1. 两个进程访问同一 VA，必须落到不同物理帧
#    期望日志同时打印 (pid, va, frame)，frame 不同
# 2. vm 服务故意拒绝一次（容量耗尽），进程必须收到明确错误而不是挂死
# 3. 原 host 测试仍全绿：make -C micro/kernel/memory test
#    （证明"无 pager 默认路径"没被破坏）
```

## 非目标

- 不做 COW、swap、反向映射、VMA 合并（`README` 明确列为不做）
- 不做多级页表的大页（Sv39 大页仍未实现，`as.c:179` 会返回 `MEM_EINVAL`）
- 不做页置换 / 缺页统计 / 访问位回收
- 不做 SMP TLB shootdown
- 不做 ASID（`as_activate` 固定 ASID 0，切换必 `sfence.vma`）

## 已知风险

- **死锁**：缺页 → IPC 到 vm 服务 → vm 服务自己也需要内存 → 也缺页。必须保证 vm 服务的 region 与栈**预映射**，且它不通过自己处理缺页。这是微内核 VM server 的经典坑，design 文档必须写出规避方案。
- **阻塞在 trap 上下文里的语义**：`as_fault` 现在在 trap 里被同步调用。改成阻塞需要保存 trap frame 并在恢复时重新执行出错指令——`closure.c` 已有 `sepc` 不推进的先例（缺页不推进 `sepc`），要确认 `sret` 回来时状态机正确。
- **`as_fault` 的 `MEM_U` 与共享内核映射**：`memory/README.md` 不变量 11/12 提到共享内核映射对 `MEM_U` 返回 `MEM_EPERM`。vm 服务补的页带 `U`，别踩这条。

## 纪律

必读：王道 3.1、3.2；汤小丹第 6、7 章。  
冲突时：`kernel/memory/README.md` 的 12 条不变量为准；想改必须先改 README 并说明理由。  
完成后：只提交本 Goal 范围 diff，附 `docs/evidence/M6-<日期>.log`（含两进程不同帧 + 拒绝路径 + host 测试全绿）。
