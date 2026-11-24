# 在小新电脑上观察静态链接与动态链接

这个实验让同一个 `add(2, 3)` 分别经过三条路径，在本机看到 ELF 信息、打开共享库的时机、系统调用和函数地址。实验运行在宿主机的 Linux 用户态，阅读内核源码来解释内核参与的部分；它不是在给 `mono` 内核增加动态链接功能。

| 程序 | `add` 从哪里来 | `libcalc.so` 被请求的时机 |
| --- | --- | --- |
| `static` | 构建时从 `libcalc.a` 选入可执行文件 | 本例运行时不需要它 |
| `startup` | 可执行文件记录依赖，由动态链接器处理 | 进入 `main` 前 |
| `runtime` | `main` 调用 `dlopen` 和 `dlsym` | 程序运行到 `dlopen` 时 |

三者都输出 `add(2,3) = 5`。`runtime` 自身仍会在启动时动态链接 libc；表格说的是 **`libcalc.so`** 的处理时机。

## 先动手观察

需要 Linux x86-64、C 编译器、GNU binutils、Python 3 和静态 libc 开发文件。`make trace` 另需 `strace` 及允许跟踪子进程的环境；GDB 是可选的。

```bash
cd /home/jerry/Projects/from-scratch/operating-systems-lab/ex/elf-linking
make
make check       # ELF 信息、正常执行、缺少共享库三项验证
make trace       # 跟踪 execve / openat / mmap / mprotect / write
```

构建产物和可重新生成的跟踪记录在本目录 `build/` 中，并由本目录的 `.gitignore` 排除。`make check` 在独立临时目录中模拟缺少 `libcalc.so`，不会移走或删除真正的库。若系统其他位置提供了同名库，动态链接器可能找到它，使这项缺库验证不再成立；`LD_LIBRARY_PATH` 等影响搜索的变量会在验证子进程中清除。

`make trace` 的完整记录在 `build/traces/`。在本机一次运行里，关键顺序是：

```text
static:  execve → [main] started → add(2,3) = 5
startup: execve → openat(libcalc.so) → [main] started → add(2,3) = 5
runtime: execve → [main] started → [main] before dlopen
               → openat(libcalc.so) → add(2,3) = 5
```

这里省略了其他库的打开和映射。`strace` 展示系统调用及其顺序，不能单独证明内核中的每个内部函数都已被探针跟踪。实际一次记录、GDB 地址和缺库结果见 [小新电脑的实验记录](evidence/xiaoxin-2026-11-23.md)。

## 第一步：静态链接时，谁接好了函数？

[`calc.c`](calc.c) 定义 `add`；[`direct.c`](direct.c) 声明并调用它。`Makefile` 先将 `calc.c` 编译成目标文件，用 `ar` 放进 `libcalc.a`，再让编译器驱动链接器生成静态链接的 `build/static`。

```text
direct.c → 目标模块 ─┐
                     ├→ 用户态链接器 ld → build/static
libcalc.a ───────────┘
```

链接器从静态库中选出需要的目标模块并处理引用；不是必须复制 `.a` 中的全部内容。查看产物：

```bash
nm build/static | rg ' add$'
objdump -d -M intel --disassemble=main build/static
readelf -lW build/static
```

这次构建的 `main` 中是对可执行文件内 `add` 的直接调用。启动时内核仍需要装入这个 ELF；只是该例不必为 `add` 打开 `libcalc.so`。链接器读取、写入文件会使用系统调用；`add` 作为普通函数执行时没有为加法主动发起系统调用。

## 第二步：启动时动态链接，内核先交给谁？

`build/startup` 中的 `add` 在构建时保留为动态符号引用。查看 ELF：

```bash
readelf -lW build/startup | rg 'INTERP|Requesting'
readelf -dW build/startup | rg 'NEEDED|BIND_NOW'
readelf -rW build/startup | rg 'add|JUMP_SLOT'
objdump -d -M intel --disassemble=main build/startup
```

本机结果有 `PT_INTERP`，路径为 `/lib64/ld-linux-x86-64.so.2`；`DT_NEEDED` 中有 `libcalc.so` 和 `libc.so.6`。实验使用 `-z now`，因此 `add` 的 PLT 绑定在进入 `main` 前完成。

```text
execve("startup")                         用户态 → 内核态
  → ELF 装入器建立主程序映射
  → 发现 PT_INTERP，装入 ld-linux
  → 设置入口并返回用户态
  → ld-linux 查找依赖、请求映射、处理符号和重定位
  → 程序入口 → main
```

具体看本机内核版本对应的重建源码：[`execve` 的入口](kernel-source/fs/exec.c)进入 `do_execve`；[`load_elf_binary`](kernel-source/fs/binfmt_elf.c) 识别 ELF、读取 `PT_INTERP`、通过 `load_elf_interp` 处理解释器，最后设置用户态入口。同文件的 `elf_map` 调用 `vm_mmap` 建立映射；[`mm/mmap.c`](kernel-source/mm/mmap.c) 中有映射处理逻辑。**主程序及解释器在内核处理 `execve` 时建立的内部映射，不会逐个显示成用户态发起的 `mmap` 系统调用。**

接下来执行的 `ld-linux` 是**用户态动态链接器**，它为了打开 `libcalc.so`、映射其段而发起 `openat`、`mmap` 系统调用。它根据 ELF 的符号和重定位信息把引用接好；内核提供文件、虚拟内存和权限机制，不替它逐个按名字解析 `add`。glibc 2.39 上游源码可对照 [`rtld.c` 的依赖加载](https://github.com/bminor/glibc/blob/glibc-2.39/elf/rtld.c#L1811-L1818)与[重定位](https://github.com/bminor/glibc/blob/glibc-2.39/elf/rtld.c#L2120-L2140)；本机 Ubuntu glibc 带发行版补丁，不能把上游源码逐字节当作本机安装包源码。

在本机反汇编中，`main` 对 `add` 的调用先到 `add@plt`。这一实验的 PLT 入口从 GOT 对应槽取出实际地址，再跳到库中。一次 GDB 观察到的真实地址如下，地址因 ASLR 每次会变化：

```text
libcalc.so 基址        0x7c7204032000
库内 add 的 ELF 偏移   0x10f9
add 的运行时地址       0x7c72040330f9
GOT 槽中保存的值       0x7c72040330f9
```

CPU 在这里执行的是普通的 `call`、读地址与跳转；它不按函数名做链接。使用其他链接选项时，函数符号也可能延迟到首次调用时绑定；这与共享库是否已经在启动时映射是两个问题。

可以在自己的下一次运行中重新观察 GOT 槽，而不需要套用上面的地址：

```text
gdb -q build/startup
(gdb) start
(gdb) x/gx &'add@got.plt'
(gdb) p/x &add
(gdb) info proc mappings
```

当前工具链中，前两条地址查询显示的目标值相同。若其他工具链未暴露 `add@got.plt` 这个 GDB 符号，可先用 `readelf -rW` 找到 `add` 的重定位槽偏移，再结合主程序装入基址计算槽地址。

## 第三步：运行时动态加载，什么时候进入内核？

[`demand.c`](demand.c) 在进入 `main` 后调用：

```c
void *h = dlopen("libcalc.so", RTLD_NOW | RTLD_LOCAL);
int (*add)(int, int) = (int (*)(int, int))dlsym(h, "add");
```

动态链接器根据文件名查找库；需要打开和映射文件时，分别发起 `openat` 和 `mmap`，内核处理后回到用户态。动态链接器处理库的依赖、重定位和初始化；`dlsym` 按名称找到导出符号并返回函数地址，程序通过函数指针调用它。glibc 上游可看 [`dl-open.c`](https://github.com/bminor/glibc/blob/glibc-2.39/elf/dl-open.c#L490-L533) 与 [`dl-load.c`](https://github.com/bminor/glibc/blob/glibc-2.39/elf/dl-load.c#L1167-L1174)。

`RTLD_NOW` 要求装入时处理必要的未定义符号；调用者之后仍通过 `dlsym` 获取 `add` 地址。调用 `dlclose` 后，不能假定所有情况下库都立即卸载，更不能继续使用已失效的函数指针。

## 和自己的 Mono OS 对照

[`mono/userland/link.ld`](../../mono/userland/link.ld) 把用户程序入口布局在固定虚拟地址；构建流程链接用户程序、转成 flat 镜像并嵌入内核。`sys_exec` 按名字寻找内嵌镜像，[`load_image`](../../mono/kernel/proc/user.c) 分配页、复制镜像并建立页表。相同虚拟地址不意味着所有进程共用相同物理页。

这条 Mono 用户程序路径目前没有通用 ELF 动态链接器或 `dlopen`。RISC-V 的堆区有按需分配；用户镜像在 `load_image` 中逐页分配和复制，不能把堆的按需分配说成整个可执行镜像都按需装入。

你的 RV32 SystemC 模拟平台是另一条路径：宿主侧的 [`elf32.hpp`](../../../computer-organization-lab/simulation/boot/elf32.hpp) 读取固定地址的静态 RISC-V ELF32，拒绝包含动态解释器或动态段的文件。这与 Mono 按名字装入内嵌 flat 镜像是两种不同的实验，不能把两者视为已经组合运行。

## 源码来源与边界

实验机当时运行 `6.14.0-37-generic`；`/proc/version_signature` 为 `Ubuntu 6.14.0-37.37~24.04.1-generic 6.14.11`，安装的 libc 为 `2.39-0ubuntu8.9`。本目录的 [`kernel-source/`](kernel-source/) 只包含本题相关的四个内核文件，不是可构建的完整内核。

这四个文件从 Linux `v6.14` 对应文件和 [Ubuntu 该版本源码包](https://packages.ubuntu.com/source/noble/linux-hwe-6.14)的发行版 diff 重建：有关文件的补丁无偏移、无 fuzz 应用；下载的 diff SHA-256 与官方 `.dsc` 一致。Ubuntu 元数据中原始包 SHA-256 与 [kernel.org 发布的 `linux-6.14.tar.gz` 校验值](https://cdn.kernel.org/pub/linux/kernel/v6.x/sha256sums.asc)一致。我们没有下载完整原始包逐文件比对，也没有重新编译正在运行的内核。目录保留了 `.dsc` 和提取的相关 patch 供核对；这些源码保留原 SPDX 许可证标记。

## 自己判断一次

在不含 `libcalc.so` 的目录中运行三个程序，哪个能成功？另外两个分别会在进入 `main` 前，还是运行到 `dlopen` 后失败？先写出预测，再运行 `make check`。然后换一个问题：库已映射、`add` 已绑定、没有缺页或异步中断时，调用 `add` 本身是否必须进入内核？
