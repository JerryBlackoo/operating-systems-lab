# 中断/异常处理汇编代码
#
# 实现用户态和内核态之间的上下文切换。
# 参考 rCore-Tutorial 实现。

.altmacro

# 保存通用寄存器的宏
.macro SAVE_GP n
    sd x\n, \n*8(sp)
.endm

# 恢复通用寄存器的宏
.macro LOAD_GP n
    ld x\n, \n*8(sp)
.endm

    .section .text
    .globl __alltraps
    .globl __commit_and_restore
    .globl __restore
    .align 2

# Trap 入口点
# 当发生中断或异常时，CPU 会跳转到这里
__alltraps:
    # 交换 sp 和 sscratch
    # 进入前: sp = 用户栈, sscratch = 内核栈
    # 交换后: sp = 内核栈, sscratch = 用户栈
    csrrw sp, sscratch, sp
    
    # 在内核栈上分配 TrapContext 空间
    # TrapContext 大小 = 32*8 (通用寄存器) + 6*8 (sstatus, sepc, kernel_satp, kernel_sp, trap_handler, kernel_tp)
    # = 38 * 8 = 304 字节
    addi sp, sp, -38*8
    
    # 保存通用寄存器 x1-x31 (跳过 x0，它始终为 0)
    sd x1, 1*8(sp)
    # 跳过 x2 (sp)，稍后保存用户栈指针
    sd x3, 3*8(sp)
    # 保存 x4 (tp) - 线程指针，用户程序可能使用
    sd x4, 4*8(sp)
    
    # 保存 x5-x31
    .set n, 5
    .rept 27
        SAVE_GP %n
        .set n, n+1
    .endr

    # 用户态 trap 已切到内核栈：立即把 stvec 切到内核态入口。
    # 标准 xv6 协议：用户入口只负责一次 sp/sscratch 交换，之后内核处理期间
    # 任何嵌套同步异常都必须走 __trap_entry（直接用内核 sp），否则会再次交换
    # 内核 sp 与 sscratch 中的用户 sp，栈被换错后形成 trap 风暴（RV BuildStorm
    # SMP=8 实测：一个 hart 卡在 __alltraps 首条指令，其余 hart 堆在它持有的
    # PROCESS_TABLE 上）。t0 已在上方保存，可安全使用。
    lla t0, __trap_entry
    csrw stvec, t0

    # 恢复内核 tp（= cpu id）。用户态会把 tp 用作 TLS 指针，进入
    # 内核后必须把 tp 恢复为当前 hart 的 cpu id，否则 per-cpu 缓存
    # （如 alloc_small/dealloc_small）会被错误索引。本字段由 __restore
    # 在上次 sret 前写入，位于固定栈位跨用户运行存活。
    # 已保存用户 tp (x4) 到 4*8(sp)，此处可安全覆写 tp。
    ld tp, 37*8(sp)
    
    # 现在可以使用 t0/t1/t2 了，因为它们已经保存
    
    # 读取并保存 sstatus 和 sepc
    csrr t0, sstatus
    csrr t1, sepc
    sd t0, 32*8(sp)    # sstatus
    sd t1, 33*8(sp)    # sepc
    
    # 读取用户栈指针（从 sscratch）并保存
    csrr t2, sscratch
    sd t2, 2*8(sp)     # 用户 sp
    # 加载内核 satp（如果需要切换页表）
    # 注意：在简单实现中，我们可能使用相同的页表
    # ld t0, 34*8(sp)  # kernel_satp
    # csrw satp, t0
    # sfence.vma
    
    # 设置 trap_handler 的参数
    # a0 = TrapContext 指针
    mv a0, sp
    
    # 调用 Rust trap 处理函数
    call trap_handler_rust
    
    # trap_handler_rust 返回后，a0 包含 TrapContext 指针

__commit_and_restore:
    # 在发布 next current 前先切到目标内核栈。保存 a0 的 16 字节位于目标
    # TrapContext 下方，finalizer 返回后恢复原指针并进入现有恢复体。
    mv sp, a0
    addi sp, sp, -16
    sd a0, 0(sp)
    call finish_staged_user_return
    ld a0, 0(sp)
    addi sp, sp, 16
    # finalizer 已结束内核态收尾；此刻才允许用户态 trap 入口接管。此前
    # 必须保持 __trap_entry：finalizer 仍可能发生内核页错误/中断，若 stvec
    # 还是 __alltraps，会把内核栈与 sscratch 里的用户 sp 交换，走错栈并把
    # 内核 ra 写进 kernel_tp（RV BuildStorm SMP=8 实测：TLB drain 读到
    # kernel_tp=ra 后越界 panic）。
    lla t0, __alltraps
    csrw stvec, t0

# 从 Trap 返回
# 参数: a0 = TrapContext 指针
__restore:
    # a0 指向 TrapContext
    mv sp, a0

    # 在加载用户 tp(x4) 之前，先把当前内核 tp（= 当前 hart 的
    # cpu id，此时仍为内核值）存入 kernel_tp 字段，供 __alltraps
    # 下次从用户态进入时 reload。__restore 运行在即将运行该
    # 进程的同一 hart 上，故 cpu id 始终与下次 trap 的 hart 一致
    # （迁移安全）。
    sd tp, 37*8(sp)
    
    # 恢复 sstatus 和 sepc
    ld t0, 32*8(sp)    # sstatus
    ld t1, 33*8(sp)    # sepc
    ld t2, 2*8(sp)     # 用户 sp
    
    csrw sstatus, t0
    csrw sepc, t1
    csrw sscratch, t2  # 保存用户栈到 sscratch
    # 恢复通用寄存器 (跳过 x0, x2)
    ld x1, 1*8(sp)
    ld x3, 3*8(sp)
    ld x4, 4*8(sp)
    
    # 恢复 x5-x31
    .set n, 5
    .rept 27
        LOAD_GP %n
        .set n, n+1
    .endr
    
    # 释放 TrapContext 空间
    addi sp, sp, 38*8
    
    # ===== DEBUG: 交换前打印 "SR" =====
    # 此时还在内核栈，t 寄存器已恢复但还没交换 sp
    # 不能使用 t 寄存器，用 sscratch 临时
    # 跳过这个调试，太复杂
    # ===== END DEBUG =====
    
    # 交换 sp 和 sscratch
    csrrw sp, sscratch, sp
    
    # 返回用户态
    sret

# 旧的 trap 入口（保留兼容性）
    .globl __trap_entry
    .align 4
__trap_entry:
    # 保存所有寄存器到栈
    addi sp, sp, -256
    
    # 保存通用寄存器
    sd x1, 8(sp)
    sd x3, 24(sp)
    sd x4, 32(sp)
    sd x5, 40(sp)
    sd x6, 48(sp)
    sd x7, 56(sp)
    sd x8, 64(sp)
    sd x9, 72(sp)
    sd x10, 80(sp)
    sd x11, 88(sp)
    sd x12, 96(sp)
    sd x13, 104(sp)
    sd x14, 112(sp)
    sd x15, 120(sp)
    sd x16, 128(sp)
    sd x17, 136(sp)
    sd x18, 144(sp)
    sd x19, 152(sp)
    sd x20, 160(sp)
    sd x21, 168(sp)
    sd x22, 176(sp)
    sd x23, 184(sp)
    sd x24, 192(sp)
    sd x25, 200(sp)
    sd x26, 208(sp)
    sd x27, 216(sp)
    sd x28, 224(sp)
    sd x29, 232(sp)
    sd x30, 240(sp)
    sd x31, 248(sp)
    
    # 读取 CSR 寄存器
    csrr a0, scause
    csrr a1, stval
    csrr a2, sepc
    
    # 调用 Rust 处理函数
    call trap_handler
    
    # 返回值是新的 sepc
    csrw sepc, a0
    
    # 恢复通用寄存器
    ld x1, 8(sp)
    ld x3, 24(sp)
    ld x4, 32(sp)
    ld x5, 40(sp)
    ld x6, 48(sp)
    ld x7, 56(sp)
    ld x8, 64(sp)
    ld x9, 72(sp)
    ld x10, 80(sp)
    ld x11, 88(sp)
    ld x12, 96(sp)
    ld x13, 104(sp)
    ld x14, 112(sp)
    ld x15, 120(sp)
    ld x16, 128(sp)
    ld x17, 136(sp)
    ld x18, 144(sp)
    ld x19, 152(sp)
    ld x20, 160(sp)
    ld x21, 168(sp)
    ld x22, 176(sp)
    ld x23, 184(sp)
    ld x24, 192(sp)
    ld x25, 200(sp)
    ld x26, 208(sp)
    ld x27, 216(sp)
    ld x28, 224(sp)
    ld x29, 232(sp)
    ld x30, 240(sp)
    ld x31, 248(sp)
    
    addi sp, sp, 256
    
    # 返回
    sret
