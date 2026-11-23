# objects：内核对象基座

本目录只实现微内核最小模型里的“名词层”：对象身份、类型、生命周期。
它不包含调度、映射或 IPC 语义；这些分别属于 `sched/`、`memory/`、`ipc/`。

其他路径的代码没有修改；本目录可以独立编译和自测。

## 四模块关系

```text
objects = 名词：有什么、活多久      （本目录）
memory  = 空间：VA -> 物理帧         （../memory/README.md）
sched   = 时间：谁在 CPU 上          （../sched/README.md）
ipc     = 关系：谁和谁原子地传消息   （../ipc/README.md）
```

最小微内核不定义“进程”；`Process = AddressSpace + Thread[] + CapSpace`
是用户态的捆绑概念。四个模块只通过三个缝连接：

```text
ref/unref    对象生命周期
block/wake   线程阻塞与唤醒
map/unmap    地址空间映射
```

除了这三个缝，四个目录之间没有其他耦合。

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
- 本模块不实现 capability/rights。权限属于“引用”而不是“对象”，后续 capability
  模块应在 `(object, rights)` 这一层扩展；不要往 `struct object` 里塞权限位。

## 边界

不做：动态内存分配、GC、循环引用回收、权限撤销（revoke）、对象继承体系。
最小模型只保证**类型安全、生命周期安全、有限资源**。
