# M0 基线设计记录

- **基线日期**：2026-11-24
- **工具链**：Ubuntu clang 18.1.3；QEMU 8.2.2；`ld.lld` / `llvm-size`

## 实测摘要

| 检查 | 结果 |
| --- | --- |
| `make -C micro run` | 退出码 0 |
| `make -C micro run BOOT_MODULES=all` | 退出码 0 |
| `make -C micro/shared check` | 退出码 0 |
| host memory / sched / ipc | `all tests passed` |
| host objects（修复前） | `object_test.c:164` 失败 |

## objects 自测修复（测试错，非实现错）

`object_lookup()` 在持锁下 `object_ref()`，返回的指针带**额外引用**（`kernel/objects/README.md`）。

- `test_lock_hook`：`object_ref()` 后再 `object_lookup()`，须三次 `object_unref` 才析构。
- `test_init_generation`：`object_unref(found)` 之后还须 `object_unref(&a->obj)` 释放 register 引用。

## M0 交付

- `kernel/objects/object_test.c` 引用归还
- `micro/Makefile` 目标 `check`
- `micro/README.md`、本文件、`docs/evidence/README.md`

## 取舍

- 不移动既有 `qemu-*.log`，只索引
- `check` 不跑 `kernel/capability` 等无 host test 的目录（与 `shared/check` 分工）