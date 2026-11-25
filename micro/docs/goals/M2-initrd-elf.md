# [M2] initrd 与最小 ELF 加载器

**依赖**：M1  
**建议 Agent**：A2  
**波次**：3

## 背景

现在的用户程序根本不是"用户镜像"：

- `kernel/syscall/user_prog.S` 是**链接在内核 ELF 里的代码模板**，启动时被逐字节拷进一个物理帧再映射到两个 AS 的同一 VA（`closure.c` 注释已明说"不能出现任何需要重定位的符号引用"）。
- `BOOT_REGION_INITRD` 在 `bootinfo.h` 里有类型，但 `kernel/boot/README.md:121` 标注"保留，当前未使用"。
- `userland/libs/demo.c` 标记 `user=true`，但走的还是 `demo_enter_user()` 直接构造 trap frame，代码来自内核镜像。
- `micro/tools/` 是**空目录**。

结论：目前"用户态"只是特权级切换的证据，不是"载入一个独立编译的镜像"。

## 教材对照

| 王道 | 汤小丹 |
| --- | --- |
| 1.5 中断、异常和系统调用（用户态入口） | 1.4 操作系统引导与运行 |
| 1.6 操作系统的运行环境 | 2.2 进程控制（程序装入） |

## 目标

1. **构建真正的用户 ELF**：新增 `userland/init` 的独立链接（现有 `userland/init/Makefile` 走的是 `demo.mk`，产物是**内核** ELF，不是用户 ELF）。要求：
   - 用户 ELF 是 `ET_EXEC`、固定基址（建议 `0x40000000`，与 `closure.c` 的 `USER_CODE_VA` 一致）、`PT_LOAD` 段页对齐；
   - 单独的输出目录（如 `build/user/init.elf`），并且**不被链接进 kernel ELF**——验收要用 `readelf`/`llvm-readelf` 证明 kernel ELF 里没有 init 的符号。
2. **打包 initrd**：`micro/tools/mkinitrd.py`（标准库，无第三方依赖）把 `build/user/*.elf` 打成一个简单归档（自定义 TAR-like 或极简 cpio 均可，**格式写进 README**），由 QEMU `-initrd` 传入；`BootInfo` 增加 `BOOT_REGION_INITRD` 的 base/size 并真正填充。
3. **最小 ELF 加载器**：`kernel/loader/`（新目录）实现：
   - 校验 `EI_MAG`/`EI_CLASS`/`EI_DATA`/`e_machine=EM_RISCV`/`e_type=ET_EXEC`；
   - 只处理 `PT_LOAD`，`p_vaddr`/`p_memsz`/`p_filesz` 页对齐校验（不要求段本身对齐，但要处理部分页）；
   - 每个段 `frame_alloc` + `as_map`（权限由 `p_flags` 映射到 `MEM_R/W/X`，**W 与 X 不可同时置位**则按 `MEM_EINVAL` 拒绝）；
   - `.bss`（`p_memsz > p_filesz` 的部分）清零；
   - 建立用户栈映射并写初始参数；
   - 失败路径必须 `frame_unref` 回滚，不许泄漏（参考 `as_map` 的失败语义：调用者仍持有自己的引用）。
4. **首个用户程序**：`userland/init/` 里新增一个**不依赖 demo 框架**的 `_start` 路径（可以是新的 `user_hello.c` + 极简启动汇编），经 `ecall` 打印 `[init] hello from initrd` 并 `THREAD_EXIT`。原来 `user=true` 的 demo 路径**保留**。
5. **`BOOT_REGION_INITRD` 消费链打通**：`boot → kernel_init → loader_load(initrd, "init") → as_create + thread_create → sched`。

## 交付

- `userland/init/`（用户 ELF 构建 + 最小 `_start`）
- `micro/tools/mkinitrd.py`
- `kernel/loader/{loader.c,loader.h,elf.h}`（新）
- `kernel/boot/bootinfo.{c,h}`（initrd region 填充）
- `micro/Makefile`（`user` 目标、`-initrd` 传给 QEMU）
- `kernel/boot/README.md` 的「不做」表里删掉"自举模块来源…可调整"那条的 initrd 部分
- `docs/design/M2-initrd-elf.md`、`docs/evidence/M2-<日期>.log`

## 验收

```bash
make -C micro user                    # 产出 build/user/init.elf
llvm-readelf -h build/user/init.elf   # Type: EXEC, Machine: RISC-V
llvm-readelf -s build/riscv/kernel.elf | rg "init_main" && echo "FAIL: 用户符号进了内核" || echo "OK"
timeout 10s make -C micro run
# 期望日志：
#   [loader] initrd=<base> size=<n>
#   [loader] init: 2 PT_LOAD, entry=0x40000000
#   [init] hello from initrd
# 且 EXIT=124（内核仍常驻，M1 的 idle）
```

负向验收（必须做，否则加载器不可信）：

```bash
# 用 tools/mkinitrd.py 打一个 e_machine 错误的 ELF，断言加载器拒绝且不 panic
# 期望：[loader] reject: bad machine，内核继续进入 idle
```

## 非目标

- 不做动态链接、不做 `ET_DYN`/PIE、不做重定位
- 不做 `argv/envp` 完整传递（先只传一个指针或什么都不传，写进 design 文档）
- 不做内核堆 `kmalloc`：加载器需要的内存全是帧（页表页 + 数据页），若确实需要小块内核内存，优先用静态池并在 design 文档说明容量上限
- 不改 `closure.c` 的现有取证路径（M3 才处理）

## 已知风险

- **`as_map_kernel_range` 的非拥有映射**：用户 AS 通过 `as_share_kernel` 共享内核顶层表项。加载器把段映射到 `0x40000000` 时若与 `KERNEL_VPN2`（`0x80000000` → index 2）冲突会 `MEM_EEXIST`。选用户基址时必须核对 Sv39 顶层索引。
- **`p_filesz` 跨页**：`as_copyout` 支持跨页，但源数据在 initrd 里可能未页对齐，必须逐页或按 len 循环，不许假设对齐。

## 纪律

必读：王道 1.5、1.6；汤小丹 1.4、2.2。  
冲突时：`kernel/memory/README.md` 的 `as_map` 失败语义（失败后调用者仍持有引用，需自行 `frame_unref`）为准。  
完成后：只提交本 Goal 范围 diff，附 `docs/evidence/M2-<日期>.log`（含正向 + 负向两条）。
