# shared/

内核、用户态服务/驱动和工具共用的 ABI 定义与编码约定。

原则只有一条：**接口定义只有一份**。系统调用号、消息布局、能力位一旦需要跨越内核与用户态的边界，
就必须写在这里；内核的 `kernel/syscall/`、`kernel/ipc/`、`kernel/capability/` 和用户态库
只能引用，不得各自复制一套或重新编号。

## 目录用途

| 路径 | 用途 | 状态 |
| --- | --- | --- |
| `shared/abi/` | 内核与用户态之间唯一的 ABI 来源 | 已定义 syscall.h |
| `shared/abi/syscall.h` | 系统调用号、参数顺序、返回值与错误约定 | 已实现 G1；编号见文件 |
| `shared/abi/message.h` | IPC 消息头/消息体布局、对齐、长度上限 | 待写 |
| `shared/abi/capability.h` | 能力类型、对象类型编号、权限位 | 待写 |

约束：

- `shared/` 只放头文件和文档，不放 `.c` / `.cpp` / `.S`，不参与生成目标文件。
- `tools/` 如果需要生成表、桩代码或文档，只读取这里的定义，不反向定义 ABI。
- 某一侧内部使用的约定（内核私有结构、服务内部协议）不要放进来；
  放进对应目录并注明“非 ABI”。
- 第一版不定义具体编号和布局。系统调用模型尚未定型，过早冻结 ABI 的代价比晚一点定义更高。

## 编码风格

这些头文件会同时被内核（`-ffreestanding`、`-nostdlib`）、用户态和工具包含，因此：

1. **语言**：C11，仅头文件。只允许 `<stdint.h>`、`<stddef.h>`、`<stdbool.h>`；
   不调用函数、不定义全局变量、不做内存分配。
2. **C/C++ 兼容**：所有声明包在下面的保护中。内核已经有 `.cpp` 文件
   （`kernel/capability/cspace.cpp`），C++ 侧必须能直接包含这些头文件。

   ```c
   #ifdef __cplusplus
   extern "C" {
   #endif
   /* ... */
   #ifdef __cplusplus
   }
   #endif
   ```

3. **类型**：跨态字段只用 `uint8_t` / `uint16_t` / `uint32_t` / `uint64_t` / `uintptr_t`。
   不用 `int`、`long`、`size_t`、`bool`，不用位域，不用编译器相关的 `#pragma pack`。
   枚举常量只作为常量使用，需要存储时用 `uint32_t`。
4. **命名**：
   - 宏和枚举常量：`OS_` 前缀 + 大写下划线，例如 `OS_SYS_IPC_SEND`、`OS_CAP_READ`；
   - 结构体：`struct os_msg_header`，需要 typedef 时用 `os_msg_header_t`；
   - 字段：小写下划线，例如 `msg_len`；
   - 所有 ABI 名字都带 `OS_` / `os_` 前缀，避免和用户态 libc、C++ 标准库冲突。
5. **数值**：每个常量显式写出值（十六进制，例如 `0x0100`），不依赖自动递增；
   删除的编号保留并标记为 `reserved`，绝不复用。
6. **布局**：字段使用自然对齐，结构体总大小显式补齐到 8 字节；
   每个跨态结构体配 `_Static_assert(sizeof(struct ...) == N, "...")`，
   有偏移要求的再加 `offsetof` 断言。指针不进入跨态结构体，
   用 `uint64_t` 地址或整数句柄代替。
7. **字节序**：目标平台 rv64 为小端，ABI 按小端直接传输，暂不定义字节序转换函数。
8. **包含方式**：仓库根目录在 include path 中，统一写
   `#include "shared/abi/syscall.h"`，不用 `../` 相对路径。
9. **头文件保护**：`SHARED_ABI_<文件名>_H`，例如 `SHARED_ABI_SYSCALL_H`。
10. **注释**：写清单位、取值范围、所有权/生命周期。排版与现有内核代码一致：
    4 空格缩进，大括号独占一行，行宽不超过 100 列。

## 示例（仅示意风格，当前不参与编译）

```c
#ifndef SHARED_ABI_SYSCALL_H
#define SHARED_ABI_SYSCALL_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define OS_ABI_VERSION 1u

/* 系统调用号从 0x0000 起；删除的编号保留，不复用。 */
enum {
    OS_SYS_NOP = 0x0000,
};

#ifdef __cplusplus
}
#endif

#endif /* SHARED_ABI_SYSCALL_H */
```

## 改动规则

- **只增不改**：已定义的编号、字段顺序/偏移、权限位含义不再变化。
- 新增系统调用/消息类型/能力位时追加新编号；废弃项标记 `reserved` 并写注释说明。
- 必须修改既有布局或语义时，视为不兼容变更：提升 `OS_ABI_VERSION`，内核与用户态同步升级，
  不允许静默改动。
- 修改 `shared/abi/` 后，内核与用户态必须一起重建，不要只更新一侧。

## 与构建系统的关系

当前 `Makefile` 只构建内核，还没有把仓库根目录加入 include path。等第一个 ABI 头文件落地时，
在 `CFLAGS` 中加 `-I.`，后续用户态构建使用同样的包含路径。
