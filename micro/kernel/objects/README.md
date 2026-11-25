# objects：内核对象基座

本目录只实现微内核最小模型里的「名词层」：对象身份、类型、生命周期。
它不包含调度、映射或 IPC 语义；这些分别属于 `sched/`、`memory/`、`ipc/`。

其他路径的代码没有修改；本目录可以独立编译和自测。

## 本模块的对象

| 对象 | 定义处 | 作用 | 生命周期由谁驱动 |
| --- | --- | --- | --- |
| `struct object` | `object.h` | 所有内核对象的公共头（必须放在第一个字段）：类型、代际 id、原子引用计数、析构回调 | 本模块（`object_register` / `object_unref`） |
| 对象注册表 `slots[]` | `object.c` | 固定大小槽位表，把 `id` 映射到对象指针；槽位复用时递增 generation | 本模块 |
| `enum object_type` | `object.h` | 对象类型标签：`OBJ_THREAD` / `OBJ_AS` / `OBJ_ENDPOINT` / `OBJ_REPLY` | 类型模块注册时填入，之后不可变 |
| 锁钩子 | `object_set_lock()` | 把注册表的临界区交给外部锁（resident 下是 `sched_lock`） | 由 `sched_init()` 安装 |

`struct object` 只有元数据，没有负载；负载由类型模块自己的静态池提供
（线程池、AS 池、endpoint 池、reply 池），本模块只保管一个指针。

## 与其他模块的关联

```text
                    ┌──────────── objects（本模块）────────────┐
                    │  id / type / refcnt / destroy 分发        │
                    └───────▲─────────▲─────────▲──────────────┘
      object_register()     │         │         │    object_lookup(id, T)
                            │         │         │
        ┌───────────────────┴──┐  ┌───┴──────┐  └──┬──────────────┐
        │ sched (OBJ_THREAD)   │  │ memory   │     │ ipc          │
        │ thread_create()      │  │ (OBJ_AS) │     │ (OBJ_ENDPOINT│
        │                      │  │as_create │     │  OBJ_REPLY)  │
        └──────────────────────┘  └──────────┘     └──────────────┘
```

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `sched` | **提供锁**，并注册线程对象 | `sched_init()` 调 `object_set_lock(sched_lock, sched_unlock)`；`thread_create()` 注册 `OBJ_THREAD`，`thread_destroy()` 由本模块在引用归零时回调 |
| `memory` | 注册地址空间对象 | `as_create()` 注册 `OBJ_AS`；`as_destroy()` 是它的析构回调 |
| `ipc` | 注册 endpoint 与 reply | `ep_create()` 注册 `OBJ_ENDPOINT`；`reply_alloc()` 注册 `OBJ_REPLY` |
| `capability` | **不直接耦合** | capability 用 `cap_oid_t` 自己编号，与 `object id` 是两套标识；二者由 `kernel/boot/service_boot.c` 在装配时对应（同一个 payload 指针） |
| `sched_thread_by_id_locked()` | 反向便利接口 | 按 object id 在 TCB 池里查线程，供 pager watchdog 在定时器中断里定位缺页线程 |
| `pager` / `pm` / `vm` | 通过 id 定位线程 | 都用 thread object id 作为跨模块的稳定标识 |

四个基础模块只通过三个缝连接：

```text
ref/unref    对象生命周期      ← 本模块
block/wake   线程阻塞与唤醒    ← sched
map/unmap    地址空间映射      ← memory
```

除这三个缝外没有其他耦合。最小微内核不定义「进程」；
`Process = AddressSpace + Thread[] + CapSpace` 是用户态的捆绑概念。

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 只有静态池 | `sched` / `memory` / `ipc` 各自维护裸指针，模块间直接互相 `#include` 取结构体 | 单模块 demo 能跑，但任何一方改结构体都会打破另外两方 |
| 2. 抽出对象头 | 引入 `struct object` 作为公共前缀，`object_register()` 分配槽位与代际 id | 需要一个**跨模块稳定的标识**：pager 要按 tid 找线程、PM 要按 tid discard、`ABI_PID_SELF` 要返回 id |
| 3. 代际 id 防悬空 | 槽位复用时 `generations[slot]++`，旧 id 查找必然失败 | 对象销毁后旧 id 仍可能被用户态持有；没有代际就会出现「新对象被旧 id 命中」 |
| 4. 锁钩子外置 | `object_set_lock()` 让 `sched_init()` 注入 `sched_lock` | 对象注册表与调度器必须共用同一把单核锁，否则 `lookup` 的「查表 + 加引用」会与析构竞争 |
| 5. 析构在锁外 | `object_release()` 先摘槽位、解锁，再调 `destroy` | 析构回调会释放类型自己的池（甚至间接触发 `as_unref`），持锁回调会造成锁序倒置 |
| 6. 弱符号扩展点 | `loader_release_launch_for_thread` / `ipc_release_thread` 作为 `thread_destroy` 的弱钩子 | `sched` 不能反向依赖 loader/ipc，但线程销毁时必须归还 launch 槽、清理 endpoint 队列里的悬空引用 |

## 最小模型

```text
Object = (type, state, refcount, payload)

FREE --object_register--> LIVE --object_unref(refcnt=0)--> DYING --> FREE
```

通用对象头：

```c
struct object {
    enum object_type type;   /* 不可变 */
    uint32_t refcnt;         /* 原子引用计数 */
    uint32_t id;             /* (generation << 16) | slot */
    uint16_t slot;
    uint16_t gen;
    object_destroy_fn destroy;
};
```

所有内核对象把 `struct object` 放在第一个字段。类型模块（Thread、AddressSpace、
Endpoint、Reply）用静态池分配自己的负载，然后调用 `object_register()` 注册。

## 不变量

1. 对象创建后 `type` 不变；状态迁移只能由类型模块的操作触发。
2. `refcnt` 等于存活引用数；归零后 `destroy` 恰好执行一次。
3. 对象从注册表摘除后才执行析构，析构回调可以释放类型自己的池。
4. 用户可见句柄是 `(id, generation)`；槽位复用会递增 generation，旧 id 查找必然失败。
5. `object_ref()` 不会复活 `refcnt == 0` 的对象。
6. 对象图禁止环；引用计数不处理循环引用，类型析构必须主动断链。
7. `object_register()` 拒绝同一对象头占用两个槽位——否则旧 id 与析构会指向同一块内存。

## 代码

| 文件 | 内容 |
|---|---|
| `object.h` | 对象头、类型枚举、注册/引用/查找 API |
| `object.c` | 静态槽位注册表、代际 id、原子引用计数、析构分发 |
| `object_test.c` | 生命周期、代际复用、类型过滤、容量边界自测 |
| `Makefile` | 主机自测构建 |

## API

```c
void object_system_init(void);
void object_set_lock(object_lock_fn lock, object_unlock_fn unlock);

int  object_register(struct object *obj, enum object_type type,
                     object_destroy_fn destroy);
void object_ref(struct object *obj);
void object_unref(struct object *obj);

struct object *object_lookup(uint32_t id, enum object_type type);
enum object_type object_get_type(const struct object *obj);
uint32_t object_get_id(const struct object *obj);
uint32_t object_refcount(const struct object *obj);
bool object_alive(const struct object *obj);
```

`object_lookup()` 返回一个额外引用，调用者必须 `object_unref()`。

## 构建与自测

```sh
make -C micro/kernel/objects test
```

预期输出：

```text
objects: lifecycle ok
objects: generation ok
objects: lock hook ok
objects: duplicate registration ok
objects: generation across reset ok
objects: capacity ok
objects: all tests passed
```

## 与 QEMU / 内核接线

```sh
make -C micro run BOOT_MODULES=objects
```

自举 demo 校验类型、最后一次引用释放和陈旧对象 ID。

- 启动早期调用 `object_system_init()`，此后才能注册对象。
- `sched_init()` 会把对象注册表锁接到 `sched_lock/sched_unlock`，也就是单核上
  关/开 `sstatus.SIE`；引用计数本身使用 `__atomic` 内建，不依赖这把锁。
- 主机自测不安装锁钩子，注册表操作是单线程的。
- 本模块不实现 capability/rights。权限属于「引用」而不是「对象」，capability
  模块在 `(object, rights)` 这一层扩展；不要往 `struct object` 里塞权限位。

## 边界

不做：动态内存分配、GC、循环引用回收、权限撤销（revoke）、对象继承体系。
最小模型只保证**类型安全、生命周期安全、有限资源**。

容量：`OBJECT_MAX_SLOTS` 个对象、16 个线程（`SCHED_MAX_THREADS`）。耗尽时
`object_register()` 返回 -1，调用方（`thread_create` / `as_create` / `ep_create`）
必须把它转成自己的错误码并回收已取得的资源。
