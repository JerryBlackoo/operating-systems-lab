# loader：initrd（MIRD）、ELF64 装载与进程启动

本目录把「initrd 里的一个 ELF 成员」变成一个能跑的 U-mode 线程：解析 MIRD
容器、校验 ELF、建立地址空间、拷入各 PT_LOAD 段、映射用户栈、创建线程并
装入起始 trap 帧。

它是内核里唯一知道「用户程序长什么样」的模块，但不理解任何服务语义：
装的是 fs 还是 shell，对 loader 没有区别。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct loader_image` | `loader.h` | 装载结果：`as`（目标地址空间）、`entry`（`e_entry`）、`stack_top`（初始 sp） |
| `struct loader_boot_args` | `loader.h` | 传给新进程的六个启动寄存器（`cap_sid` + `a0..a5`）；服务用它们接收 ready/recv/sm 句柄 |
| `struct user_launch` | `loader.c` | 一个已装载待运行的进程：`frame`（构造好的 trap 帧）+ `kstack[]`（该线程的内核栈） |
| `g_launches[]` / `g_launch_used[]` | `loader.c` | `USER_LAUNCH_MAX = 16` 的静态 launch 槽池 |
| `struct elf64_ehdr` / `elf64_phdr` | `loader.c` | ELF64 头与程序头（本地定义，不依赖 libc） |
| MIRD v1 容器 | `loader.c` | initrd 打包格式：魔数 + 版本 + 成员表（名字、偏移、长度），4 字节对齐 |
| `initrd_image` / `initrd_image_size` | `loader.c` | `loader_pin_initrd` 把 BootInfo 里的 initrd 区间钉在帧池之外，避免后续 `frame_alloc` 覆写 |
| `enum loader_status` | `loader.h` | `LOADER_OK` / `EINVAL` / `ENOENT` / `ENOMEM` / `EFULL` / `EMAP`，PM 据此回复真实错误码 |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `memory` | 建立地址空间 | `as_create` / `as_share_kernel` / `as_map`（PT_LOAD 与栈）/ `as_map_kernel_range`（设备 MMIO）/ `as_translate` / `as_activate` |
| `objects` / `sched` | 创建线程 | `thread_create(as, user_launch_entry, launch, kstack, ...)`；`sched_thread_set_cap_sid` 设置身份 |
| `capability` | AS 的 cap 绑定 | `cap_as_bind(as)`——只 bind，不再替 VM 装 cap（A2 之后由 PM 显式 grant） |
| `boot/bootinfo` / `boot/fdt_initrd` | 输入来源 | `loader_pin_initrd(bi)`；`initrd_base` / `initrd_size` |
| `boot/kernel_init` | 启动期调用方 | 依次 `loader_spawn_user` 拉起 sm/pm/vm/heaptest/heaptest2/init |
| `boot/kernel_spawn` | PM spawn 调用方 | `loader_spawn_user` + `loader_abort_spawn` / `loader_terminate_spawn` |
| `arch/riscv`（trap） | 首次运行 | `user_launch_entry` 把 `sp` 换成 launch 的 trap 帧后 `j trap_return`，靠 `sret` 进入 U-mode |
| `storage` / `virtio_blk` | 设备 MMIO | `loader_map_kernel_devices` 把 UART/PLIC/virtio 的非拥有恒等映射复制进每个用户 AS |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 只有 MIRD 解析 | `loader_find_initrd` 校验魔数、版本、成员边界、4 字节对齐 | initrd 是唯一能拿到用户程序的地方，先把容器解析做对 |
| 2. ELF 校验 | `validate_elf`：ELF64 LE RISC-V **ET_EXEC**；`p_filesz <= p_memsz`；`p_offset` 与 `p_vaddr` 页内偏移一致；entry 必须落在可执行 PT_LOAD | 一个坏镜像不能让内核崩；`ET_DYN` 需要重定位，本模型不做 |
| 3. W^X 与页权限并集 | `page_rights()` 合并同页所有段，拒绝 `W|X` | 两个 PT_LOAD 可能共享一页，简单按段覆盖会丢掉权限 |
| 4. 映射与回滚 | `map_segments` 先清零再映射；失败时 `unmap_segments` 逐段撤销 | 映像损坏时不能留下半个地址空间 |
| 5. 共享内核上半区 | `as_share_kernel(as, root, 2)` | 用户 AS 在 satp 切换后仍需进入 S 态 trap handler、访问内核代码与帧池 |
| 6. 设备 MMIO 也要映射 | `loader_map_kernel_devices` 复制 UART/PLIC/virtio 的非拥有恒等映射 | 0x0c/0x10 段的 MMIO 不在内核 RAM 上半区；不复制就会在 U-mode 代执行 syscall 时缺页 |
| 7. 用户栈 | `map_user_stack`；页数从 **1 页** 扩到 **4 页**（A1b） | ext4+vfs 搬进 U-mode `fs` 后，挂载与路径解析的调用深度在 4 KiB 栈上溢出，表现是栈顶附近缺页被 VM 以 `reason=rights` 拒绝、进程死在 mount 前 |
| 8. launch 槽与内核栈 | `user_launch` 自带 `kstack`；`USER_LAUNCH_MAX = 16` | 每个用户线程需要一个内核栈来承载它的 trap 帧与 syscall 执行 |
| 9. 失败回滚 | `loader_abort_spawn`（未运行）/ `loader_terminate_spawn`（已运行，可延迟） | PM spawn 后续步骤（VM grant、进程表）失败时必须撤回已建立的一切 |
| 10. launch 槽归还时机（A12） | 不再在 abort 时释放，改由 `thread_destroy` 弱钩子 `loader_release_launch_for_thread` 归还 | RUNNING 线程还在用那块内核栈；提前释放会让下一次 spawn 复用同一块内存并踩坏它 |
| 11. 不再替 VM 装 cap | `loader` 只 `cap_as_bind`；boot 直 spawn 由 `kernel_init` 调 `cap_vm_ensure_as_map` | A2 之前 loader 直接 `cap_root_install` 给 VM，绕过了 PM 的显式授权链 |

## API

- `loader_pin_initrd(bi)`：登记 BootInfo 中已保留的 initrd 物理区间；重复登记不同区间会失败。
- `loader_find_initrd(base,size,name,...)`：只读解析 MIRD v1，校验版本、成员边界与 4 字节对齐。
- `loader_load_elf(as,elf,size,out)`：向新的空用户 AS 装载 ET_EXEC；要求 entry 落在可执行 PT_LOAD。
- `loader_spawn_user(...)`：共享内核页表上半区、建立用户栈和线程，并安装启动 SID/寄存器。
- `loader_abort_spawn(t)`：撤回**尚未运行**用户入口的 spawn（内部 `sched_thread_discard`）。
- `loader_terminate_spawn(t)`：终止**可能已经跑起来**的子进程（内部 `sched_thread_terminate`，RUNNING 时登记延迟退出）。

## 不变量

1. 只接受 ELF64 little-endian RISC-V ET_EXEC；只映射 PT_LOAD。
2. `p_filesz <= p_memsz`，文件和地址计算不得溢出；`p_offset` 与 `p_vaddr` 页内偏移一致。
3. 页面权限是该页所有段的并集；任何 W+X 页面都拒绝。
4. 新页先清零再映射，bss 不靠镜像文件内容补齐。
5. 映射失败回滚本次段页；调用方销毁 AS 后帧计数必须恢复。
6. `make test` 使用 host 帧/AS 实现与合成 ELF/MIRD；只有 host 测试使用合成输入。
7. launch 槽（含内核栈）只在线程**真正销毁**时归还，不能在请求终止时提前释放。

## 已知边界

不做 ET_DYN/PIE、动态链接、重定位、完整 argv/envp。`USER_LAUNCH_MAX = 16`，
capability 镜像用 9 个（sm/pm/vm/heaptest/heaptest2/init/fs/console/shell）。

resident 下每个服务独立 AS；内核代其执行 syscall 时仍运行在该服务的 satp 下，
所以 virtio/UART/PLIC 的 MMIO 必须复制进每个用户 AS——这是
`loader_map_kernel_devices` 存在的原因，也是「微内核里设备 MMIO 归属」
这个问题的当前答案。

QEMU 验收用真实 initrd 成员，不是 fixture token：`initrd` 目标由
`tools/mkinitrd.py` 打包，`loader_find_initrd` 按名字取成员。
