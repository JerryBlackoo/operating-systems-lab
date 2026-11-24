# capability：能力表与派生树

本目录只回答两个问题：

1. **谁能做什么？** —— 某个主体持有某个 cap 时，能对某个对象执行哪个操作。
2. **权限怎么流动？** —— cap 如何派生、转移、撤销，对象何时释放。

它不定义对象语义、IPC、内存映射和调度。对象层通过 `struct cap_type` /
`struct cap_object_ops` 注入类型、权限上限、操作要求与 `invoke` / `finalize`。

一句话模型：

> 每个主体一张私有表；表项 = `(对象, 权限)`；对象只能经表项被命名；
> 新表项只能由旧表项按权限子集派生或转移；对象寿命 = 表项引用计数；
> 撤销 = 删除派生子树。

## 文件

| 文件 | 职责 |
| --- | --- |
| `capability.h` | 公共契约：句柄编码、权限位、`cap_type` / `cap_object`、错误码 |
| `capability.c` | 最小模型实现：CSpace、CDT、权限检查、生命周期、`revoke` |
| `cap_platform.h` | 能力模块与机器层的唯一内部契约（关/开中断） |
| `cap_arch_riscv.c` | RISC-V `sstatus.SIE` 保存/恢复 |
| `cap_selftest.c` | 内核内自检：测试对象 + 断言 + SBI 输出（正式镜像可去掉） |
| `demo.c` | 组合运行示例：把 cap 接到 objects / ipc / memory / sched / syscall |
| `Makefile` | 只做核心文件的 RISC-V 交叉编译检查 |

## 边界

| 做 | 不做 | 归属 |
| --- | --- | --- |
| cap 的表示、槽位、权限检查 | endpoint 的 send/recv | `kernel/ipc` |
| `mint` / `copy` / `move` / `revoke` | frame 的 map/unmap | `kernel/memory` |
| 对象引用计数与 finalize 触发 | 对象内部状态机 | `kernel/objects` |
| 统一 `invoke` 入口与错误码 | 调度、超时、优先级 | `kernel/sched`、`kernel/clock` |
| 根能力的初始安装 | 路径解析、服务发现 | `userland` name server |
| — | 谁应该拿到哪个 cap（策略） | `init` / policy server |

判据有两句：

- **换一个对象类型（endpoint / frame / task），这段代码还需要吗？**
  需要 → 本目录；只有某类对象才这样 → 对象所属模块。
- **换一个平台（QEMU → 真实板），这段代码还需要吗？**
  需要 → 本目录；只有 RISC-V 才这样 → `cap_arch_riscv.c`。

## 九个逻辑切面

| # | 切面 | 只回答一个问题 | 模块内状态 | 去掉它会怎样 | 参考 |
| --- | --- | --- | --- | --- | --- |
| S1 | 表示面 | 用户拿到的 cap 是什么值？ | `(slot, generation)` 编码的 32 位句柄 | 用户能伪造内核指针 | Zircon handle value；Genode `Capability<T>` |
| S2 | 空间面 | cap 存在哪、怎么分配？ | 每主体 64 槽的私有表 | 无隔离，猜 id 即可引用 | Zircon per-process handle table |
| S3 | 权限面 | cap 能对对象做什么？ | `rights` 位图 + 类型上限 + 操作要求表 | 无法表达只读、不可转交 | seL4 `cap_rights_t`；Zircon `zx_rights_t` |
| S4 | 派生面 | 新 cap 从哪来？ | `mint/copy` + CDT 子节点 | 只能同权复制，无法委托子集 | seL4 `CNode_Mint/Copy` |
| S5 | 撤销面 | 如何收回已发出的权限？ | `revoke` 删子树；`delete` 删自己+子树 | 能力发出去就收不回 | seL4 `CNode_Revoke/Delete` |
| S6 | 生命周期面 | 对象何时释放？ | `cap_refs` + `pins` + `finalize` | 悬垂引用 / UAF | Zircon `Dispatcher` refcount；Linux `fput` |
| S7 | 调用面 | 一次操作如何被允许？ | `required ⊆ rights` → `ops->invoke` | 授权不可强制，靠约定 | seL4 `decodeInvocation` |
| S8 | 并发面 | 多 hart 如何一致？ | 单 hart `sstatus.SIE` 保存/恢复 | 检查与使用之间被 `revoke` 撕开 | Zircon `HandlePointer`；Linux `fdget/fput` |
| S9 | 引导面 | 第一个 cap 从哪来？ | 启动时的 root cap 安装 | 只能从 ambient authority 开始 | seL4 initial thread CSpace |

S1–S6 是状态与数据，S7 是唯一入口，S8 是正确性边界，S9 是初始条件。切面之间
不共享职责：撤销不顺手析构对象，生命周期不顺手做权限检查。

## 最小理论模型

```text
M = (O, T, D)

O : oid ⇀ (type, cap_refs, pins)      -- 对象表，内核私有
T : sid × slot ⇀ cap                  -- 每个主体一张能力表
cap = (oid, rights, badge, node)
D : node ⇀ node⊥                      -- 派生森林，⊥ 表示根

Rtype : type ⇀ rights                 -- 该类型允许的最大权限
Rop   : type × op ⇀ rights            -- 该操作要求的最小权限
```

把状态画出来：

```text
Task A · CSpace                 对象表 O                 Task B · CSpace
slot 0  cap(o7, R/W) ─┐
slot 5  cap(o7, R)   ─┼──────> o7 Endpoint  refs=3 <── cap(o7, R)  slot 0
slot 3  cap(o9, R)   ────────> o9 Frame     refs=1

派生树 CDT：
  A.slot0 (root)
    ├── A.slot5  (mint)
    └── B.slot0  (move；表项换了位置，节点和父边没变)
```

### 不变量

```text
W1  c.rights ⊆ Rtype(type(c.oid))
W2  c.oid ∈ dom(O)
W3  ∀p→c ∈ D: c.oid = p.oid ∧ c.rights ⊆ p.rights
W4  D 是森林；每个 cap 恰好一个 node；node 不共享
W5  O(o).cap_refs = |{c ∈ T | c.oid = o}|
W6  cap 只存在于 T；用户可写内存中不含 cap
```

### 转移

| 转移 | 前置 | 后置 |
| --- | --- | --- |
| `MINT(s,i→j,r,b)` | `T(s,i)=c`；j 空；`r⊆c.rights`；`r⊆Rtype`；改 badge 时要求 `GRANT` | 新 cap `(c.oid,r,b,ν)`；`parent(ν)=c.node`；`cap_refs++` |
| `COPY(s,i→j)` | j 空；源 cap 存在 | 同权同 badge；新子节点；`cap_refs++` |
| `MOVE(s,i→s',j)` | j 空；`s≠s'` 时要求 `GRANT` | 表项移动；`node` 不变；`cap_refs` 不变 |
| `REVOKE(s,i)` | `T(s,i)=c` | 摘除 `c.node` 的全部真后代；`cap_refs--`；到 0 调 finalize |
| `DELETE(s,i)` | `T(s,i)=c` | `REVOKE` + 删除自己；`cap_refs--`；到 0 调 finalize |
| `INVOKE(s,i,op,args)` | `op` 被类型支持；`Rop(type,op) ⊆ c.rights` | 对象语义；原子执行；不变量保持 |
| `FINALIZE(o)` | `cap_refs=0 ∧ pins=0` | 从 `O` 移除；恰好一次 |

### 性质

```text
P1 封闭性    没有 cap ⇒ 不可 invoke
P2 不增权    派生链上 rights 单调不增
P3 撤销完备  revoke 后任何表中都不存在该子树的后代
P4 无悬垂    有 cap ⇒ 对象存活；finalize 恰好一次
P5 不可伪造  用户只能给 slot/generation；解析只查当前主体表
P6 线性化    每个 transition 是一个原子步
```

P3 成立的前提是 `MOVE` 保留 node。若把 `MOVE` 实现成 `DELETE + MINT`，
被移走的子 cap 会变成新根，`revoke(A.slot0)` 就漏掉 `B.slot0`。这是实现里
最隐蔽的 bug。

## 为什么是最小模型

| 删掉 | 退化为 | 失去 |
| --- | --- | --- |
| `rights` | 纯句柄表 / fd 表 | 无法表达只读、只执行、不可转交 |
| 每主体私有表 | 全局对象表 | 无隔离；猜 id 即可引用 |
| `mint` 子集派生 | `dup` 同权复制 | 无法委托子集权限 |
| CDT + `revoke` | Zircon 式句柄表 | 无法收回已发出的权限 |
| `cap_refs` | 裸指针 | 悬垂引用 / UAF |
| `invoke` 守卫 | 裸指针 + 约定 | 授权不可强制 |
| `move` | 只能 `copy` | 发送方保留权限，无法移交 |
| `generation` | slot 复用 | ABA / 旧句柄复活 |

不可再少的内核机制是五条：**私有表、`(对象,权限)`、唯一解析、
`required ⊆ rights`、refcount**。CDT + `revoke` 是第六条，它把“句柄表”
升级成“可撤销的能力系统”；badge、generation、CNode 树都是可选增强。

## 实现

### 表示与空间

- 句柄是 `(generation << 16) | slot`，不是内核指针。
- 每个主体一张 `slot[64]` 平表；`CAP_SLOT_AUTO` 让派生操作选择第一个空槽。
- 槽位释放时 `generation` 加一；旧句柄再次查找会得到 `CAP_E_BAD_HANDLE`。
- `slot[64]` 的 `used` 位图放在一个 `uint64_t` 里；当前最多 64 槽。
- 所有表都是静态数组，不在 `invoke` 路径上分配内存。

### 权限与派生

- 权限位：`READ`、`WRITE`、`GRANT`、`MAP`、`EXEC`。
- 每个 `cap_type` 给出 `rights_mask`、`supported` 操作位图、`required[op]`。
- `mint` 必须满足 `r ⊆ 源 rights` 且 `r ⊆ 类型 rights_mask`；否则 `CAP_E_RIGHTS`。
- `copy` 等价于同权同 badge 的派生，但不要求 `GRANT`。
- 改 badge 视为一次委托，要求源 cap 带 `GRANT`。
- 跨主体 `move` 要求源 cap 带 `GRANT`；同主体换槽不要求。
- 派生出的 cap 必有一个父节点；`copy` 也会建子节点，否则撤销无法区分副本。

### 撤销与生命周期

- `revoke` 扫描所有主体、所有槽位，摘掉目标节点的全部真后代；因此跨
  `CSpace` 的 `move` 也能被收回。
- 摘完 cap 后，再从目标节点的 `first_child` 递归释放整棵子树。
- `delete` = `revoke(后代)` + 删除目标自己；不会留下孤儿节点。
- 对象 `cap_refs` 记录指向它的 cap 数；最后一个 cap 消失后调用
  `ops->finalize`，并把对象移出对象表。
- `cap_system_init()` 是启动/测试用的全量重置，不是运行期操作。

### 调用与并发

`cap_invoke` 是唯一入口：

```text
查表 → 对象存活 → op 受支持 → required ⊆ rights
     → pins++ → 放锁 → ops->invoke → 加锁 → pins-- → 可能 finalize
```

- `pins` 保证“检查”和“使用”之间对象不会被 `revoke`/`delete` 终结。
- 单 hart 上 `cap_arch_intr_save/restore` 就是全局互斥；`cap_arch_riscv.c`
  只碰 `sstatus.SIE`。
- `finalize` 在锁内调用；对象层不得在 `finalize` 里回调能力模块。真实内核
  若要允许回调，应改成延迟回收。
- `cap_invoke` 在调用对象 ops 时暂时放锁；对象 ops 可以在放锁窗口内调用
  常规能力 API，但不得调用 `cap_system_init()`（全量重置）。

### 引导

`cap_object_create()` 和 `cap_root_install()` 是最小 bootstrap 接口，不是
用户 ABI。真实系统里对象创建属于 `objects/untyped`，能力模块只接收“新对象”
和“初始 root cap”。用户态入口只应看到 `mint/copy/move/delete/revoke/invoke/info`。

### 与其他模块的组合

`demo.c` 是组合根，不把对象语义搬进能力模块。它做三件事：

1. 用 `struct cap_type` 给 endpoint / AS / frame / reply 注入类型、权限上限
   和操作要求；
2. 用 `cap_root_install` / `mint` / `move` 把权限分给两个主体；
3. 实现 `struct syscall_backend`，让一次 U-mode 形状的 `ecall` 先经过
   句柄解析与权限检查，再落到 ipc / memory 的对象原语。

| 对象 | `cap_type` | `rights_mask` | 单对象操作 | 语义归属 |
| --- | --- | --- | --- | --- |
| endpoint | `CAP_TYPE_ENDPOINT` | R / W / Grant | `SEND`、`RECV` | `kernel/ipc` |
| address space | `CAP_TYPE_AS` | `MAP` | 复合 map/unmap | `kernel/memory` |
| frame | `CAP_TYPE_FRAME` | R / W / X / `MAP` | 复合 map | `kernel/memory` |
| reply | `CAP_TYPE_REPLY` | W | `SEND`（单次） | `kernel/ipc` |

```text
client CSpace                         server CSpace
slot 0  ep root (R/W/Grant)           slot 0  ep recv (R/Grant)
slot 2  ep send (W)  ──┐                   ▲
slot 3  as (MAP)       ├─ endpoint ────────┘   (mint + move，节点不换)
slot 4  frame (R/W/MAP)│
                       └─ cap_acquire → syscall 后端 → ipc / as_map
```

- **单对象操作**走 `cap_invoke`：查表 → 类型支持 → `required ⊆ rights` →
  `ops->invoke`。endpoint 的 `SEND/RECV` 就是这条路。
- **跨两个 cap 的复合操作**走 `cap_acquire`：syscall 后端按 `SYSCALL_AS` /
  `SYSCALL_FRAME` 分别借出 AS 与 frame，两者都 pin 住后再调用 `as_map` /
  `as_unmap`，最后 `cap_release`。这保持了“能力模块不硬编码对象关系”。
- **reply 权限不是 bearer token**：服务端 `RECV` 到带 `reply_id` 的请求时，
  后端为该 token 创建一把 `CAP_TYPE_REPLY` cap 并装进服务端 CSpace；服务端
  用这把 cap 应答。应答成功后后端 `cap_delete` 收回它，第二次使用得到
  `CAP_E_BAD_HANDLE`。`reply_holder.consumed` 再兜底 `CAP_E_BUSY`。
- **撤销跨 CSpace**：服务端的 recv cap 是客户端根 cap 的后代（`mint` 后
  `move`，节点不动），所以 `cap_revoke` 根 cap 会同时摘掉客户端的 send cap
  和服务端的 recv cap。
- **寿命**：endpoint / AS / frame 的 cap 对象在 `finalize` 里分别
  `ep_unref` / `as_unref` / `frame_unref`。最后一个 cap 删除前对象一直存活；
  正在 `cap_invoke` 或 `cap_acquire` 中的对象由 `pins` 保护。

调用链：

```text
trap frame (scause=8)
  -> syscall_dispatch          参数形状、句柄位宽、地址对齐
  -> cap_backend.acquire       按 kind 解析 cap、检查 rights、pin 对象
  -> cap_backend.invoke        ipc / memory 原语（CALL 会阻塞到 reply）
  -> cap_backend.release      unpin；最后一个 cap 已删除时在此 finalize
```

这条链在 QEMU 中由两个内核线程跑通：客户端 `IPC_CALL`，服务端 `RECV` 后
用 reply cap 应答；客户端随后 `AS_MAP` / `AS_UNMAP`。它验证了锁序：
`cap_arch_intr_save` 只覆盖表操作，`cap_invoke` / `cap_acquire` 在调用对象
原语前放锁，因此 IPC 阻塞不会占着能力锁。

## API

| API | 作用 | 关键错误 |
| --- | --- | --- |
| `cap_system_init()` | 清空对象表、CSpace、CDT | — |
| `cap_object_create(type,payload,&oid)` | 创建对象（bootstrap） | `CAP_E_BAD_ARG`、`CAP_E_NOMEM` |
| `cap_object_discard(oid)` | 丢弃无引用的 bootstrap 对象 | `CAP_E_BUSY` |
| `cap_root_install(...)` | 安装根 cap（bootstrap） | `CAP_E_OCCUPIED`、`CAP_E_RIGHTS` |
| `cap_mint(sid,src,dst,r,b,&out)` | 减权/改 badge 派生 | `CAP_E_RIGHTS`、`CAP_E_OCCUPIED` |
| `cap_copy(sid,src,dst,&out)` | 同权复制 | `CAP_E_OCCUPIED` |
| `cap_move(from,src,to,dst,&out)` | 转移 cap；保留节点 | `CAP_E_RIGHTS`（缺 `GRANT`） |
| `cap_delete(sid,h)` | 删自己 + 后代 | `CAP_E_BAD_HANDLE` |
| `cap_revoke(sid,h)` | 只删后代 | `CAP_E_BAD_HANDLE` |
| `cap_invoke(sid,h,op,args)` | 单对象操作的唯一调用入口 | `CAP_E_RIGHTS`、`CAP_E_TYPE` |
| `cap_acquire(sid,h,rights,&obj)` | 内核内借用：检查权限并 pin 对象 | `CAP_E_RIGHTS`、`CAP_E_BAD_OBJECT` |
| `cap_release(obj)` | 与 `cap_acquire` 配对；最后一个引用消失时 finalize | — |
| `cap_info(sid,h,&info)` | 只读元数据，不暴露对象指针 | `CAP_E_BAD_HANDLE` |
| `cap_check_invariants()` | 检查 W1–W6（含 W3 父边同对象且不减权）与计数一致性 | `CAP_E_INVARIANTS` |
| `cap_live_caps()` / `cap_live_objects()` / `cap_object_refs()` / `cap_object_pins()` | 自检观测，不改变状态 | — |

`CAP_E_BAD_HANDLE` 统一表示“槽位不存在、generation 不匹配、或主体不匹配”；
不区分“对象不存在”和“无权访问”，避免泄露其他主体的状态。

## QEMU / RISC-V 映射

| 模型 | 实现 |
| --- | --- |
| 原子 transition | `cap_arch_intr_save/restore`：`csrrc/csrs sstatus, SIE` |
| 私有 CSpace | 8 × 64 的静态 `g_cspaces[]` |
| 对象表 | 64 个静态 `g_objects[]`，`oid = index + 1` |
| CDT | 512 个静态节点，`parent / first_child / next_sibling` |
| 对象终结 | `cap_refs == 0 && pins == 0` 时调 `ops->finalize` |
| 对象语义注入 | `demo.c` 的 `endpoint_type` / `as_type` / `frame_type` / `reply_type` |
| 句柄解析 | `cap_backend.acquire`：`cap_acquire` + 类型 + rights，返回 pin |
| 复合操作 | `AS_MAP` 同时借出 AS cap 与 frame cap，再调 `as_map` |
| 能力语义 | 与 MMU/页表无关；MMU 只负责把内核表挡在用户态之外 |

单 hart、`-smp 1` 起步。SMP 时把 `cap_arch_intr_save/restore` 换成
“关中断 + 每 CSpace 自旋锁”，并固定锁序：**先 CSpace，后对象**。

## 构建与验证

模块级交叉编译检查（不链接、不依赖 boot）：

```sh
cd micro/kernel/capability
make          # 生成 micro/build/capability/{capability,cap_arch_riscv}.o
make clean
```

完整 QEMU 验证：

```sh
cd micro
make run BOOT_MODULES=capability
```

自举清单执行 `capability` 时先跑 `cap_selftest()`，再跑与 other modules 的
组合用例。QEMU 输出摘要如下。

```text
[capability] start

[cap] capability minimal model
[cap] object create  OK
[cap] root install  OK
[cap] mint subset  OK
[cap] mint superset rejected  OK
[cap] badge mint needs grant  OK
[cap] badge mint with grant  OK
[cap] copy  OK
[cap] cross-task move needs grant  OK
[cap] cross-task move with grant  OK
[cap] moved source handle stale  OK
[cap] revoke root  OK
[cap] root survives revoke  OK
[cap] mint child gone  OK
[cap] copy child gone  OK
[cap] cross-task child gone  OK
[cap] refs back to root  OK
[cap] delete root  OK
[cap] finalized once  OK
...
[cap] acquire pins object  OK
[cap] delete defers finalize  OK
[cap] release finalizes  OK
[cap] 100 mint/delete cycles  OK
[cap] all objects finalized  OK
[cap] no live caps  OK
[cap] no live objects  OK
[cap] invariants  OK
[cap] live caps=0 objects=0 finalized=5/5
[cap] selftest ok
capability: caps gate IPC call/reply, AS map and cross-CSpace revoke
[capability] PASS
[bootstrap] PASS
[kmain] shutting down
```

### 验收表

| 输出 / 用例 | 证明 |
| --- | --- |
| `mint subset OK` / `mint superset rejected` | 派生只减权，不增权 |
| `badge mint needs grant` | badge 变更受 `GRANT` 约束 |
| `badge mint with grant` | 带 `GRANT` 的委托路径成立 |
| `cross-task move needs grant` | 没有 `GRANT` 不能把 cap 交给别的主体 |
| `cross-task move with grant` | 有 `GRANT` 的转移成立 |
| `moved source handle stale` | 源槽释放后 generation 失效 |
| `revoke root` + `root survives revoke` | `revoke` 只删后代，不删目标 |
| `mint/copy child gone` | 派生树的后代被完整收回 |
| `cross-task child gone` | `move` 保留节点，撤销跨 CSpace 有效 |
| `refs back to root` | 撤销后引用计数回到目标 cap |
| `delete root` + `finalized once` | 最后一个 cap 删除时 finalize 恰好一次 |
| `slot reuse new gen` / `deleted handle stale` | 槽位复用不会复活旧句柄 |
| `task isolation` | 同一数字在不同主体表中无意义 |
| `invoke without write right` | 调用前统一检查 `required ⊆ rights` |
| `invoke with read right` | 合法调用到达对象 ops |
| `unsupported op rejected` | 类型不支持的操作返回 `CAP_E_TYPE` |
| `occupied slot rejected` / `unchanged` | 目标槽占用时失败是原子的 |
| `acquire rejects missing right` | `cap_acquire` 在借出对象前检查权限 |
| `acquire pins object` / `release finalizes` | `pins` 保护 check-use 窗口 |
| `delete defers finalize` | 最后一个 cap 删除时，在途对象不析构 |
| `100 mint/delete cycles` | 无槽位/节点泄漏 |
| `live caps=0 objects=0` | 清理后所有引用与对象归零 |
| `invariants OK` | W1–W6（含 W3 父边）与计数一致 |
| `capability: caps gate IPC ...` | syscall / IPC / AS map / 跨 CSpace 撤销贯通 |

## 已知限制

- **单 hart**：全局关中断即互斥；SMP 需要每 CSpace 锁和锁序。
- **平表**：64 槽/主体，不支持 seL4 CNode 树、guard、动态扩展。
- **静态上限**：8 主体、64 对象、512 节点；`cap_object_create` 用尽返回 `CAP_E_NOMEM`。
- **撤销是 O(主体×槽位)**：当前 8×64，规模变大应改成每对象后代链或 CNode 树。
- **`finalize` 在锁内**：对象层不得回调能力 API；真实内核应延迟回收。
- **`cap_system_init` 是全量重置**：只允许启动/测试时调用。
- **generation 16 位**：同一槽位释放 65536 次后可能回绕；真实系统应扩大或加随机基址。
- **没有 untyped/retype**：`cap_object_create` 只是 bootstrap 替身，对象创建归 `objects/untyped`。
- **syscall 后端在 S-mode 自测**：`demo.c` 用 `scause=8` 的陷阱帧驱动真实
  `syscall_dispatch`，但没有真的进入 U-mode；U-mode 隔离需要 MMU 打开后的
  用户页表，属于后续镜像。
- **IPC 语义仍由 ipc 模块实现**：能力层只决定“谁可以把消息交给哪个
  endpoint”；消息缓冲、阻塞队列、reply 对象寿命仍在 `kernel/ipc`。
- **未验证**：SMP、U-mode 隔离、MMU 开启后的表保护、并发 `invoke`/`revoke`、
  `cap_acquire` 期间被 `revoke` 的语义（当前实现下已借出的 pin 仍然有效）。

## 参考

| 主题 | Linux | seL4 | Zircon / Genode |
| --- | --- | --- | --- |
| 权限位 | `kernel/capability.c`、`security/commoncap.c`（特权位，反面参照） | `cap_rights_t`：R/W/Grant/GrantReply | `zircon/rights.h` |
| 句柄表 | `fs/file.c`、`include/linux/fdtable.h` | `src/object/cnode.c`、`src/kernel/cspace.c` | `handle_table.cpp`、`handle.cpp` |
| 派生/撤销 | 无通用机制（`dup` 同权；`revoke(2)` 仅 tty） | `src/kernel/cdt.c`（旧版 `mdb.c`） | Zircon 无；Genode `base/include/base/capability.h` |
| 传递 | `net/core/scm.c` 的 `SCM_RIGHTS` | IPC cap transfer + `Grant` | `channel_write_etc` |
| 生命周期 | `fput` / `f_count` | `finaliseCap` | `Dispatcher` refcount |

Linux 的 `CAP_*` 只在“权限位图 + 检查点”这一面上有参考价值；不要照搬它的
ambient 语义。fd 表那一套才是结构参照，但它缺 `mint` 的权限衰减、CDT 和
`revoke`——这三样正是本目录补上的。
