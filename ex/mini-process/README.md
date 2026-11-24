# 从第一次换栈到进程隔离

写第一个版本时，先规定一条能看见的行为。让两个执行同一函数的任务轮流跑，各自记住上次跑到了哪一行。为了实现它，你只需要回答三个问题。旧任务的返回地址在哪，新任务怎样获得第一条指令，谁决定下一次把 CPU 交给谁。能回答这三个问题，就能写出本实验的第一版。

再加要求时，沿着同样的方法找新代码的归属。任务要等数据，负责数据的对象就要维护等待者。任务要共享数据或隔离数据，内核就要在恢复它时选择相应的地址空间。任务退出后还要让父进程取到退出码，进程管理就要记录父子关系和资源的回收时机。

## Linux 的拆分

Linux v6.14 里，一次创建可以沿着 `kernel_clone → copy_process → wake_up_new_task` 追。每次都要准备一份可独立执行的 `task_struct` 和内核栈。到 **资源** 这一层，`copy_mm` 根据 `CLONE_VM` 决定引用现有 `mm` 还是生成新的 `mm`。文件表和信号也各自处理共享或复制。到 **指令** 这一层，x86 的 `copy_thread` 准备第一次被调度时所需的栈帧，返回地址指向 `ret_from_fork_asm`。最后才让调度器选得到这个任务。可以用这个顺序检查自己的创建函数。对象先私下初始化，最后公开为可运行，失败时按资源取得的逆序清理。[Linux `copy_process`](https://github.com/torvalds/linux/blob/v6.14/kernel/fork.c#L2015) · [x86 `copy_thread`](https://github.com/torvalds/linux/blob/v6.14/arch/x86/kernel/process.c#L160)

同一线程组的线程有各自的 TID，共用 TGID。新进程的组长满足 `pid == tgid`。单独指定 `CLONE_VM` 表达的是共享地址空间。要加入同一线程组还要指定 `CLONE_THREAD`，源码要求它同时满足共享信号处理表和共享地址空间。[Linux 线程组赋值](https://github.com/torvalds/linux/blob/v6.14/kernel/fork.c#L2320) · [创建标志检查](https://github.com/torvalds/linux/blob/v6.14/kernel/fork.c#L2043)

再追一条贯穿目录的路径。读空管道时，`fs/pipe.c` 知道什么叫“有数据可读”，于是让调用者在管道的读等待队列上等。写入满足条件后走唤醒接口，调度器更新任务的可运行状态。下次选中它，x86 代码恢复它的内核栈，它在原来阻塞的调用路径上继续。调度器可以用同一套选任务和换任务逻辑服务管道、锁和其他等待对象。进程退出则由 `exit.c` 协调资源释放、通知和父进程的等待。[管道读写](https://github.com/torvalds/linux/blob/v6.14/fs/pipe.c#L350) · [调度与上下文切换](https://github.com/torvalds/linux/blob/v6.14/kernel/sched/core.c#L5322) · [进程退出](https://github.com/torvalds/linux/blob/v6.14/kernel/exit.c)

Torvalds 在访谈中回忆，他在 386 上先用定时器切换两个分别输出 A 和 B 的任务。后来把它们改成终端模拟器的两个方向，分别处理键盘到调制解调器、调制解调器到屏幕。要把下载的东西写到磁盘时，才加硬盘驱动和文件系统。Linux 0.01 的源码已经能看到定时器、`schedule()`、`fork` 和用 `ljmp` 触发的 386 硬件任务切换。你可以照着他验证 A/B 行为的办法起步，本实验用软件换栈实现同一个抽象。[Torvalds 本人访谈](https://arstechnica.com/information-technology/2015/08/how-linux-was-born-as-told-by-linus-torvalds-himself/) · [Linux 0.01 源码档案](https://www.kernel.org/pub/linux/kernel/Historic/linux-0.01.tar.gz)

## seL4 的拆分

seL4 的内核让持有相应 capability 的用户态代码组装线程。典型顺序是 `Untyped_Retype` 得到 TCB，准备用户栈和 IPC buffer，用 `TCB_Configure` 装入 CSpace 和 VSpace 等引用，用 `TCB_WriteRegisters` 写好初始 PC/SP，再 `TCB_Resume` 让它可调度。CSpace 是线程查找 capability 所用的空间，VSpace 是线程执行访存时所用的虚拟地址空间。在 MCS 配置下，还要有可用的调度上下文和预算。用户态构造器决定几个 TCB 共享同一 VSpace，或哪些属于不同的进程。共享 VSpace 表示共享地址映射，应用的进程分组和 `wait` 等约定由上层管理。[seL4 线程教程](https://docs.sel4.systems/Tutorials/threads.html) · [seL4 手册第 6 章](https://sel4.systems/Info/Docs/seL4-manual-latest.pdf)

把 Linux 的管道例子换成 seL4 的同步消息。调用者执行阻塞式 Send，Endpoint 上没有接收者时，`endpoint.c` 记录 `BlockedOnSend` 并把 TCB 链入该 Endpoint 的队列。接收者来到后，IPC 代码完成传输并把发送者变回可运行，`kernel/thread.c` 统一选择谁获得 CPU，架构代码恢复被选线程的用户上下文。Endpoint 决定**这条消息能否交付**，调度器决定**此刻谁拿到 CPU**。2009 年的实现采用事件式内核，等待关系显式留在对象和 TCB 上，内核可以结束这次处理并回到选中的用户线程。[seL4 `sendIPC`](https://github.com/seL4/seL4/blob/14.0.0/src/object/endpoint.c#L20) · [seL4 调度](https://github.com/seL4/seL4/blob/14.0.0/src/kernel/thread.c#L375) · [SOSP 2009 论文第 4 节](https://sel4.systems/Research/pdfs/sel4-formal-verification-os-kernel.pdf)

论文第 5.2 节记下了团队的开发顺序。他们先做了一个单地址空间、没有中断、使用简单线性页表的可执行 Haskell 内核，验证团队同时搭建证明基础。再完成内核设计、原型和 C 实现，并把证明扩到完整内核。可执行原型可以接到源自 QEMU 的模拟环境，跑用户代码，观察一次陷入前后的状态。对这个实验，可以照用这种方法。先让几个状态和转移实际跑起来，下一阶段才加入会改变这些转移的事件。[seL4 论文第 2.2 和 5.2 节](https://sel4.systems/Research/pdfs/sel4-formal-verification-os-kernel.pdf)

| 写实验时遇到的问题 | Linux | seL4 |
| --- | --- | --- |
| 下次从哪条指令继续 | 对应任务的栈、架构上下文 | TCB 的架构上下文 |
| 读写落到哪份内存 | 任务引用的 `mm` | TCB 配置的 VSpace |
| 等待条件归谁管 | 管道、锁等内核对象 | Endpoint、Notification 等内核对象 |
| 谁定义进程级创建、退出和等待 | 内核里的进程管理路径 | 需要该语义的用户态系统 |

## 在本机跑第一版

本机的 Intel Core i5-13500H 支持 32 位和 64 位 x86 模式。实验沿用已有 BIOS 启动路线，采用 32 位保护模式和一颗虚拟 CPU。机器引导、寄存器和栈的关系可以用 TCG 单步观察，同一镜像也跑过本机的 KVM。

| 交接 | 当前实现 |
| --- | --- |
| BIOS → LBA 0 | 父目录的 `boot.S` 在 `0x7c00` 执行，读 LBA 1 到 `0x7e00` |
| LBA 1 → 内核入口 | 本目录 `load-kernel.S` 按构建得到的扇区数，把 LBA 2 起的内核读到 `0x1000` |
| 汇编 → C | 复用 `x86-bios/kernel-entry.S`，安装 GDT、置 `CR0.PE`、远跳、设置 `ESP=0x7c00`、清零 `.bss` 后调用 `kmain` |

两份启动实验可以单独运行。这个子目录用 GNU Make、GNU binutils 和支持 `-m32` 的 GCC 构建。镜像与 ELF 位于 `mono/build/mini-process/`。读入的镜像及 `.bss` 都受 `0x6c00` 边界约束，为引导及调度栈留出其上到 `0x7c00` 的 4 KiB。两个任务各有一段 4 KiB 的内核栈，存在 `.bss` 内。[stage1](../x86-boot/boot.S) · [32 位入口](../x86-bios/kernel-entry.S) · [链接脚本](kernel.ld)

```sh
cd /home/jerry/Projects/from-scratch/operating-systems-lab/ex/mini-process
make run
make run QEMU=qemu-system-x86_64 ACCEL=kvm CPU_MODEL=host
```

两种方式都打印了下面的结果。两个 `local` 序列各自延续，同一个 `shared` 跨两个任务连续递增。

```text
stage1: hello from MBR (0x7C00)
kmain: 32-bit protected mode
A local=0 shared=1
B local=0 shared=2
A local=1 shared=3
B local=1 shared=4
A local=2 shared=5
B local=2 shared=6
PASS: both threads exited
```

### 先手推 `ret` 会跳到哪里

执行 `call switch_context` 时，CPU 已把恢复执行的 EIP 放在旧栈上。因为这一步是一次普通 C 调用，汇编按 i386 ABI 保存 `EBP/EBX/ESI/EDI`。本阶段从内核入口开始保持 IF=0，代码只使用通用寄存器。按压栈顺序推导，此时旧 `ESP` 指向下面五个字，从低地址向高地址排列。

```text
偏移       +0     +4     +8     +12    +16
内容       EDI    ESI    EBX    EBP    ret 要取的 EIP
```

选择任务的 C 代码把目标栈帧传给切换函数。汇编记住旧 `ESP`，再把目标栈帧装入 `ESP`，四次 `pop` 加一次 `ret` 就恢复目标执行流。**新线程也使用这条恢复路径。** 创建时在新栈顶手工填同样的五个字，第五个字填 `thread_entry`，于是第一次 `ret` 会进入它。之后它的 `yield` 会把真正的返回地址留在栈上。保存栈指针、恢复栈指针和准备初始栈帧，这是可以带到下一个架构的三步。[切换汇编](switch.S) · [初始栈帧与调度循环](kernel.c)

本版让调度器在独立的引导栈上运行。它扫描两个固定的 TCB 槽，选中 `READY` 就标成 `RUNNING` 并换到任务栈。任务主动 `yield` 回来时恢复为 `READY`，入口函数返回时标 `EXITED`，调度器下次扫到它会跳过。这样先把如何保存执行现场与如何挑下一个任务做成两段能单独验证的代码。

### 用 GDB 亲眼看第一次换栈

在本目录的第一个终端运行 `make debug`。第二个终端运行 `gdb -q -nx ../../mono/build/mini-process/kernel.elf`，输入下面的指令。

```gdb
target remote 127.0.0.1:1234
hbreak *switch_context
continue
info registers cr0 cs esp eip
x/3wx $esp
set $incoming = *(unsigned int *)($esp + 8)
x/5wx $incoming
si 8
info registers esp eip
si 5
info registers esp eip
continue
set $a_ret = *(void **)$esp
continue 3
set $incoming = *(unsigned int *)($esp + 8)
x/i $a_ret
p *(void **)($incoming + 16) == $a_ret
disable 1
detach
```

本次执行在切换入口看到 `CR0=0x11`、`CS=0x08`。执行换栈指令后，`ESP` 从调度栈落到 A 的初始帧，`ret` 后的 `EIP` 是 `thread_entry`。最后的比较返回真，目标帧第五个字等于 A 第一次 `yield` 留下的返回地址，也就是 `thread_yield` 中调用 `switch_context` 后的那条指令。地址以你这次构建的符号为准。

## 从这里继续写到真正的进程

下面的阶段以每关的行为作为交付条件。可以先在现有 `kernel.c` 写清状态和接口，等调度、陷入、内存各自有独立职责时再拆文件。

| 阶段 | 先写的接口与数据 | 在单核 QEMU 中判通过 |
| --- | --- | --- |
| 1. 协作线程，已实现 | `thread_create`、`thread_yield`、退出标记、每线程栈和选择循环 | A/B 各自恢复，两个任务都退出 |
| 2. 中断与抢占 | `idt_init`、`trap_dispatch`、PIT IRQ0、可恢复的 `trap_frame` | 用两个从不 `yield` 的任务，观察其独立计数都在增长 |
| 3. 用户线程 | ring 3 GDT 项、TSS、`int 0x80`、`iret` | GDB 中用户代码的 `CS & 3 == 3`，`SYS_PUTCHAR` 经过内核打印 |
| 4. 进程与地址空间 | 页帧分配、`vm_new/map`、`arch_activate`、`proc_spawn`、`thread_spawn` | 同进程不同线程读到同一全局变量，不同进程用同一虚拟地址读到各自的数据 |
| 5. 完整生命周期 | 阻塞队列、`thread_exit`、`proc_exit`、`waitpid`、回收路径 | 等待中的父进程让出 CPU，子退出后父拿到退出码，反复创建退出能复用槽位和栈 |

### 第 2 关把 `call` 现场扩展成中断现场

先让 `irq0` 只累加 `ticks`。在 PC 虚拟机上初始化 IDT，重映射 PIC，把 IRQ0 接到向量 `0x20`，配置 PIT，然后开放这一条 IRQ。准备异常入口后再开可屏蔽中断。看到连续 tick，再让调度器介入。处理 IRQ0 时向 PIC 发 EOI，再走可能切到别的任务的返回路径。

异步中断能打断普通指令流，现场包括被打断时的通用寄存器。32 位 x86 在同特权级进入中断门时压入 EFLAGS、CS、EIP，跨到 ring 0 时还会保存原 SS/ESP 并使用 TSS 指定的内核栈。汇编入口给没有硬件错误码的向量补 0，压向量号，保存通用寄存器和使用的段寄存器，切到内核数据段，清 DF 并对齐栈后调用 C。依据 `CS & 3` 识别帧有没有用户 SS/ESP。共享一份恢复代码，用 `iretl` 还原硬件部分。给两个初始任务也构造符合这个格式的帧，EIP 填任务入口，EFLAGS 的 bit 1 与 IF 置 1。可以把自愿让出做成一次内核软件中断，和 IRQ0 一起走同一套恢复路径。[Intel SDM 卷 3A 的中断和异常章节](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)

C 端的最小接口是 `struct trap_frame *trap_dispatch(struct trap_frame *entered)`。进来时把 `entered` 存在当前 TCB 上。时间片用完时，将当前线程按 `RUNNING → READY` 放到队尾，再选队首并标成 `RUNNING`。系统调用主动让出也可以走这个流程。返回目标帧地址，汇编用这个返回值换 `ESP` 后恢复。先保持中断门中的短 C 处理过程不可重入，调度发生在统一出口。每个可选任务都有一份完整的帧，等待者在对应等待队列中，退出者已移出可选集合。就绪集合为空时选择有独立栈的 idle。

### 第 3 关先证明权限切换发生

在现有 GDT 的内核段后面放 DPL 3 的代码段和数据段，例如用户 CS `0x1b`、用户 SS `0x23`。加载 32 位 TSS，设置 `ss0=0x10`，选中哪个用户线程就把它的内核栈顶写入 `esp0`。构造第一帧时填入用户 EIP、CS、EFLAGS、ESP、SS，令恢复的 DS 和 ES 使用用户数据段，EFLAGS 保持 IOPL=0，通过 `iretl` 进入用户代码。用一个内嵌的小程序执行 DPL 3 的 `int 0x80`，第一个 syscall 只通过寄存器传入字符。从 GDB 核对进入用户态时的 CS，再在入口处核对 CPU 已切到目标内核栈，最后核对返回的用户 EIP 与 ESP。此关证明特权级和栈切换，内存访问边界由下一关的分页建立。[Intel SDM 卷 3A 的保护与 TSS 章节](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)

### 第 4 关从一个共享页表根发展到两个

先拿 BIOS E820 核过的一段可用物理内存做 4 KiB 页帧池，预留内核、启动区和固件区。开始使用 1 MiB 以上的物理地址时，确保 A20 已打开。用传统 32 位非 PAE 两级页表，先建立内核、IDT、TSS、所有内核栈的相同映射，并限制为 supervisor 访问。再添加用户代码、数据、栈页，按需在两级页表中设置 U/S 和 R/W，写入物理页目录基址到 `CR3`，置 `CR0.PG`。在单一地址空间中让第 3 关的程序先跑一次。[Intel SDM 卷 3A 的分页章节](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)

```c
/* 后续阶段的数据结构草图。 */
struct address_space { uint32_t page_directory_pa; };
struct process { int pid; struct address_space *space; unsigned live_threads; };
struct thread {
    int tid;
    struct process *owner;
    struct trap_frame *frame;
    uintptr_t kernel_stack_top;
    enum thread_state state;
};
```

`thread_spawn` 在已有 `process->space` 内给新线程映射一段独立用户栈，再分配它的内核栈和初始帧。`proc_spawn` 先分配一份新的页目录，继承相同的 supervisor 内核映射，装载自己的用户数据和栈，再创建第一个线程。把初始化完成的对象一起发布，失败时逆序释放。切换线程时，先在中断关闭的内核中执行 `arch_activate(next)`，更新 `esp0`，页表根改变时写 `CR3`，再恢复目标帧。从旧帧执行到新帧的这段代码和栈在两份页表中都要可访问。

把数据页固定映射在 `0x40001000`。P1 的 T1 写入 11，主动交给同进程的 T2，T2 应读到 11。P2 的 T3 在同一个虚拟地址读到自己的初始值，再写入 22。切回 P1 后仍应读到 11。GDB 可以同时核对 TID、`CR3` 和两份页表条目的物理页号。再让一个用户任务尝试访问已映射的内核页，确认进入 `#PF`，`CR2` 指向被访问的地址，然后让另一个任务继续运行。

### 第 5 关给阻塞和退出安排资源归属

写一个带等待队列的事件，消息已到就消费，消息没到就让当前线程从 `RUNNING` 进入 `BLOCKED`。在单核上用同一段关中断的临界区完成检查条件和登记等待者。通知者先完成等待的操作，再把等待者移出等待队列并置回 `READY`。如果线程等在 syscall 上，把结果写入保存帧的 EAX，用户态便能从 syscall 的下一条指令继续。用生产者延迟几 tick 的案例验收，确认等待期间 CPU 仍能跑其他线程。随后把这套等待机制用在父进程的 `waitpid` 上。

退出线程先移出调度集合，然后转到仍有效的栈上回收其内核栈。进程的最后一个线程退出后，等没有 CPU 使用该地址空间再释放其页表和用户页，保留 PID 和退出码供父进程领取。父进程消费退出记录时才能复用这个 PID 槽。让父先进 `waitpid`，让子故意晚几个 tick 退出，再连续创建、退出、等待多次，观察空闲的线程槽和页帧数回到起点。

## 亲手写第一处改动

把 A 改成跑三轮，B 改成只跑一轮。动手前先写出哪一次 `ret` 会进 B 的启动入口，哪一次会回到 B 的 `yield` 后，以及 B 退出后调度器怎样识别它。第一次只改每线程轮数，观察当前对共享计数的校验如何失败。然后让校验反映 3 加 1 的预期，再用同一条 `make run` 验证 `A0 B0 A1 A2`，用 GDB 核对 B 退出后没有再换到它的栈。
