# [M0] 基线冻结与一条命令自检

**依赖**：无  
**建议 Agent**：A0  
**波次**：1

## 背景

`micro/` 现在有 20 个模块、13 个单模块 QEMU 镜像，但：

- 没有 `micro/README.md`，新开一个会话只能靠逐个目录读 README 才能知道现状；
- 没有总 `check` 目标，验收要手敲 4 条不相关的命令，容易漏；
- `kernel/objects` 的 host 自测**当前失败**（`object_test.c:164`），但模块 README 仍写着"all tests passed"；
- 证据散落在各目录的 `qemu-*.log`，没有统一命名和索引。

本 Goal 不实现任何新能力，只把"现在的真实状态"变成可复现、可判定、可写进文档的东西。

## 教材对照

| 王道 | 汤小丹 |
| --- | --- |
| 1.1 操作系统的概念与功能 | 第 1 章 操作系统引论 |
| 1.4 操作系统的体系结构 | 1.4 操作系统引导与运行 |

本 Goal 不新增教材内容，只在 `micro/README.md` 里标注每个模块对应哪一节，供后续 Goal 引用。

## 目标

1. **修 `kernel/objects` host 自测**。
   - 根因（已核实）：`object_lookup()` 返回额外引用（`kernel/objects/README.md` 明写"调用者必须 `object_unref()`"）。`test_lock_hook` 里 `object_ref(&a->obj)` 多取一份 + `object_lookup` 再取一份，代码只 `object_unref(found)` 归还了后者，再 `object_unref(&a->obj)` 归还自身那份后 refcnt 仍为 1，析构不发生。
   - 第二处：`test_init_generation` 里 `object_register` 给的那一份未归还，`object_unref(found)` 之后 refcnt 仍为 1。
   - **必须在 commit message 写清"是测试漏归还引用，不是 `object.c` 缺陷"**，并给出 refcnt 演算。
   - 不许改 `object.c`、不许放宽断言、不许删用例。
2. **新增 `micro/check` 一条命令**：
   ```sh
   make -C micro check
   ```
   顺序：4 个 host 测试（objects / memory / sched / ipc）→ `make -C shared all`（13 个单模块镜像）→ `make -C micro BOOT_MODULES=all run`（整内核）。任一步失败即中止并打印失败项。
3. **新增 `micro/README.md`**：状态表（每模块：host 测试 / 单模块 QEMU / 真实 or 替身 / 对应教材节号）+「本树不是什么」一节 + 指向 `docs/goals/`。
4. **建立 `micro/docs/evidence/`**，把现有 4 份 log（`kernel/boot/qemu-boot-capability.log`、`qemu-boot-umode.log`、`qemu-default-preempt.log`、`kernel/syscall/qemu-unified.log`）用软链接或索引文件登记，不移动原文件（避免破坏各 README 的相对链接）。**后续状态（2026-11-25）**：`kernel/syscall/qemu-unified.log` 已归档为 `docs/evidence/qemu-unified-legacy.log`，所有引用已改指新路径。
5. **`micro/docs/design/M0-baseline.md`**：记录基线日期、工具链版本、上面 6 项检查的原始结果。

## 交付

- `kernel/objects/object_test.c`（仅两处补 `object_unref`，附注释）
- `micro/Makefile`（新增 `check` 目标）
- `micro/README.md`（新）
- `micro/docs/evidence/README.md`（证据索引）+ `micro/docs/evidence/M0-<日期>.log`
- `micro/docs/design/M0-baseline.md`
- `micro/docs/goals/README.md` 的「基线事实」表按实际结果更新

## 验收

```bash
# 1. 逐个 host 测试
make -C micro/kernel/objects test   # objects: all tests passed，退出码 0
make -C micro/kernel/memory  test
make -C micro/kernel/sched   test
make -C micro/kernel/ipc     test

# 2. 一条命令全绿
make -C micro check                 # 退出码 0

# 3. 文档存在且与实测一致
test -f micro/README.md && rg "objects" micro/README.md
```

期望：`make -C micro check` 退出码 0，且 `micro/README.md` 里 objects 行不再标红。

## 非目标

- 不新增任何内核能力（不加帧池、不加加载器、不动 `kmain`）
- 不合并/重命名现有 `qemu-*.log`
- 不改 `mono/`
- 不引入 CI 配置文件（本机 `make check` 就是唯一入口）

## 纪律

必读：王道 1.1、1.4；汤小丹 1.4。  
冲突时：`kernel/objects/README.md` 的「`object_lookup()` 返回一个额外引用」为权威契约。  
完成后：只提交本 Goal 范围 diff，附 `docs/evidence/M0-<日期>.log`。
