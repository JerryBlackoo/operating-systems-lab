# RustMicroOS 启动入口
# RISC-V 64-bit 汇编代码

.section .text.entry
.globl _start
.globl secondary_hart_start

_start:
    # OpenSBI 在 S-mode 下跳转到这里
    # a0 = hartid (由 OpenSBI 传递)
    # a1 = dtb 地址 (由 OpenSBI 传递)
    
    # 保存 hartid 和 dtb
    mv s0, a0          # s0 = hartid
    mv s1, a1          # s1 = dtb
    
    # 设置栈指针
    # 每个 hart 有独立的栈空间
    la sp, boot_stack_top
    
    # 计算当前 hart 的栈偏移
    # 每个 hart 栈大小为 128KB（64KB 在 RV SMP=8 ap_online 路径被
    # SpinLock::lock 深帧 + 嵌套 timer trap 实测压穿，Hart6/7 同时落入
    # slot8 互踩，KTRAP 记到 sp 已越界 ~400B）。
    slli t0, s0, 17    # t0 = hartid * 131072 (2^17)
    sub sp, sp, t0
    
    # 保存 hart id 到 tp 寄存器
    mv tp, s0
    
    # 简单方案：检查 boot_flag，第一个到达的 hart 启动
    # 注意：这在单核启动时是安全的，QEMU 默认只有一个 hart 先启动
    la t0, boot_flag
    lw t1, 0(t0)
    bnez t1, secondary_hart    # 如果已经是 1，进入等待
    li t1, 1
    sw t1, 0(t0)               # 设置为 1
    fence                      # 内存屏障
    
    # 成功获取启动权，继续启动
    # a0 = hartid, a1 = dtb (恢复参数)
    mv a0, s0
    mv a1, s1
    
    # 跳转到 Rust 入口
    call kernel_main

secondary_hart:
    # 其他 hart 等待 SBI IPI 唤醒
    # SBI send_ipi 会向目标 hart 投递 S-mode software interrupt (SSIP)
secondary_wait:
    wfi
    # 检查 SSIP 位
    csrr t0, sip
    andi t0, t0, 2
    beqz t0, secondary_wait

    # 清除 SSIP
    li t0, 2
    csrc sip, t0

    # 跳转到 Rust 从核入口
    # a0 = hartid
    mv a0, s0
    call secondary_hart_main
    
    # 如果返回，继续等待
    j secondary_wait

secondary_hart_start:
    mv s0, a0
    # 越界 hart 直接停机，避免它的 64KB 栈槽落到 boot_stack 之外踩坏内存
    li t1, 16
    bgeu s0, t1, secondary_wait
    la sp, boot_stack_top
    slli t0, s0, 17
    sub sp, sp, t0
    mv tp, s0
    mv a0, s0
    call secondary_hart_main
    j secondary_wait

# 启动标志
.section .data
.align 2
boot_flag:
    .word 0

# 启动栈
.section .bss.stack
.globl boot_stack
boot_stack:
    .space 131072 * 16 # 16 个 hart，每个 128KB（须 >= sched::smp::MAX_CPUS）
.globl boot_stack_top
boot_stack_top:
