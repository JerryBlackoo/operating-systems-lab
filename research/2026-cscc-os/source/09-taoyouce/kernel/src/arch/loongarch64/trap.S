# =============================================================================
# RustMicroOS — LoongArch64 异常/中断入口汇编
# LoongArch64 Trap (Exception/Interrupt) Entry Assembly
# =============================================================================
#
# 来源说明 / Source Attribution:
#   中断向量入口汇编（32 个通用寄存器 + ERA/PRMD/BADV 保存/恢复）
#   参考自 PolyHAL 项目 (https://github.com/Byte-OS/polyhal)，
#   遵循 MIT/Apache-2.0 双协议。
#   Rust trap handler 调用和微内核栈切换为 RustMicroOS 适配代码。
#
#   Interrupt vector entry assembly (32 GPR + ERA/PRMD/BADV save/restore)
#   is adapted from the PolyHAL project (https://github.com/Byte-OS/polyhal),
#   licensed under MIT/Apache-2.0 dual license.
#   Rust trap handler invocation and microkernel stack switching are
#   RustMicroOS-specific adaptation code.
#
# 参考文档 / References:
#   - 《龙芯架构参考手册》卷一：基础架构 第 7 章
#   - PolyHAL: polyhal/src/loongarch64/trap.S
#   - NPUcore-IMPACT: LoongArch64 trap 处理代码
#   - LoongArch Linux 内核: arch/loongarch/kernel/genex.S
#
# La64TrapContext 内存布局 / Memory Layout:
#   偏移 0x000: regs[0]  ($r0 = zero)
#   偏移 0x008: regs[1]  ($r1 = ra)
#   偏移 0x010: regs[2]  ($r2 = tp)
#   偏移 0x018: regs[3]  ($r3 = sp)
#   ...
#   偏移 0x0F8: regs[31] ($r31 = s8)
#   偏移 0x100: era       (异常返回地址)
#   偏移 0x108: prmd      (先前模式信息)
#   偏移 0x110: badv      (出错虚拟地址)
#   偏移 0x118: kernel_sp (内核栈指针)
#
#   总大小: 32*8 + 4*8 = 288 字节 = 0x120
#
# LoongArch64 异常处理流程 / Exception Handling Flow:
#   1. 硬件自动: CRMD.PLV→PRMD.PPLV, CRMD.IE→PRMD.PIE,
#      CRMD.PLV=0, CRMD.IE=0, PC→ERA, 跳转到 EENTRY
#   2. 软件: 保存寄存器→调用 Rust handler→恢复寄存器→ertn 返回
#
# CSR 编号 / CSR Numbers:
#   0x0  = CRMD    (当前模式信息)
#   0x1  = PRMD    (先前模式信息)
#   0x5  = ESTAT   (异常状态)
#   0x6  = ERA     (异常返回地址)
#   0x7  = BADV    (出错虚拟地址)
#   0x30 = SAVE0   (内核临时保存寄存器 0)
#   0x31 = SAVE1   (内核临时保存寄存器 1)
# =============================================================================

# La64TrapContext 结构体偏移常量
.equ CTX_SIZE,      0x130       # 304 字节 (32*8 + 6*8)
.equ CTX_REGS,      0x000       # regs[0] 偏移
.equ CTX_ERA,       0x100       # sepc/era 偏移
.equ CTX_PRMD,      0x108       # sstatus/prmd 偏移
.equ CTX_BADV,      0x110       # badv 偏移
.equ CTX_KSATP,     0x118       # kernel_satp 偏移
.equ CTX_KERNEL_SP, 0x120       # kernel_sp 偏移
.equ CTX_TRAP_HDL,  0x128       # trap_handler 偏移

    .section .text
    .globl __la64_alltraps
    .globl __la64_commit_and_restore
    .globl __la64_restore
    .align 12                   # 4KB 对齐（EENTRY 要求页对齐）

# =============================================================================
# __la64_alltraps — 异常/中断入口
# Trap Entry Point
#
# 🟢 寄存器保存/恢复框架复用自 PolyHAL（MIT/Apache-2.0）
# 🟡 微内核栈切换和 Rust handler 调用为 RustMicroOS 适配
#
# 进入时硬件状态:
#   - CRMD.PLV = 0 (内核态)
#   - CRMD.IE = 0 (中断关闭)
#   - ERA = 异常发生时的 PC
#   - PRMD = 异常发生前的 CRMD 状态
#   - $sp = 用户栈（如果从用户态陷入）或内核栈（如果从内核态陷入）
# =============================================================================
__la64_alltraps:
    # ==========================================================================
    # 第 1 步：保存用户 $sp，判断是否嵌套 trap
    # Step 1: Save user $sp, check if nested trap
    #
    # 检查 PRMD.PPLV：如果是 PLV0（内核态），说明是嵌套 trap，
    # 不从 SAVE1 加载栈，直接使用当前 $sp。
    # ==========================================================================
    csrwr       $sp, 0x30           # SAVE0 = 当前 $sp
    csrwr       $t0, 0x32           # SAVE2 = 用户 $t0，后续 CSR 读写会 clobber $t0
    csrrd       $sp, 0x1            # $sp = PRMD（临时借用）
    andi        $sp, $sp, 0x3       # $sp = PRMD.PPLV
    bnez        $sp, .Lfrom_user    # PPLV != 0 → 从用户态进入

    # 从内核态进入（嵌套 trap）：使用当前 $sp
    csrrd       $sp, 0x30           # $sp = SAVE0（恢复原始 $sp）
    addi.d      $sp, $sp, -CTX_SIZE # 在当前栈上分配 TrapContext
    b           .Lsave_regs

.Lfrom_user:
    # 从用户态进入：从 SAVE1 加载内核栈
    csrrd       $sp, 0x31           # $sp = 内核栈顶 (SAVE1)
    addi.d      $sp, $sp, -CTX_SIZE # 分配 TrapContext

.Lsave_regs:

    # ==========================================================================
    # 第 3 步：保存所有 32 个通用寄存器
    # Step 3: Save all 32 general purpose registers
    #
    # 🟢 复用自 PolyHAL 的寄存器保存框架（MIT/Apache-2.0）
    # ==========================================================================
    # $r0 (zero) — 始终为 0，但仍保存以保持结构体完整性
    st.d        $zero, $sp, CTX_REGS + 0*8     # regs[0] = $zero

    # $r1 (ra) — 返回地址
    st.d        $r1,  $sp, CTX_REGS + 1*8      # regs[1] = $ra

    # $r2 (tp) — 线程指针
    st.d        $r2,  $sp, CTX_REGS + 2*8      # regs[2] = $tp

    # $r3 (sp) — 用户栈指针（从 SAVE0 恢复）
    csrrd       $t0, 0x30                       # $t0 = 用户 $sp (从 SAVE0)
    st.d        $t0,  $sp, CTX_REGS + 3*8      # regs[3] = 用户 $sp

    # $r4-$r11 (a0-a7) — 参数/返回值寄存器
    st.d        $r4,  $sp, CTX_REGS + 4*8      # regs[4]  = $a0
    st.d        $r5,  $sp, CTX_REGS + 5*8      # regs[5]  = $a1
    st.d        $r6,  $sp, CTX_REGS + 6*8      # regs[6]  = $a2
    st.d        $r7,  $sp, CTX_REGS + 7*8      # regs[7]  = $a3
    st.d        $r8,  $sp, CTX_REGS + 8*8      # regs[8]  = $a4
    st.d        $r9,  $sp, CTX_REGS + 9*8      # regs[9]  = $a5
    st.d        $r10, $sp, CTX_REGS + 10*8     # regs[10] = $a6
    st.d        $r11, $sp, CTX_REGS + 11*8     # regs[11] = $a7

    # $r12-$r20 (t0-t8) — 临时寄存器
    # 注意: $t0 ($r12) 被上面的 CSR 读写用作临时寄存器。
    # 入口第一时间已把用户态 $t0 保存到 SAVE2，这里恢复并写入 TrapContext。
    csrrd       $t0, 0x32                       # $t0 = 原始用户 $t0
    st.d        $t0, $sp, CTX_REGS + 12*8       # regs[12] = $t0
    st.d        $r13, $sp, CTX_REGS + 13*8     # regs[13] = $t1
    st.d        $r14, $sp, CTX_REGS + 14*8     # regs[14] = $t2
    st.d        $r15, $sp, CTX_REGS + 15*8     # regs[15] = $t3
    st.d        $r16, $sp, CTX_REGS + 16*8     # regs[16] = $t4
    st.d        $r17, $sp, CTX_REGS + 17*8     # regs[17] = $t5
    st.d        $r18, $sp, CTX_REGS + 18*8     # regs[18] = $t6
    st.d        $r19, $sp, CTX_REGS + 19*8     # regs[19] = $t7
    st.d        $r20, $sp, CTX_REGS + 20*8     # regs[20] = $t8

    # $r21 (reserved)
    st.d        $r21, $sp, CTX_REGS + 21*8     # regs[21] = $reserved

    # $r22 (fp/s9) — 帧指针
    st.d        $r22, $sp, CTX_REGS + 22*8     # regs[22] = $fp

    # $r23-$r31 (s0-s8) — callee-saved 寄存器
    st.d        $r23, $sp, CTX_REGS + 23*8     # regs[23] = $s0
    st.d        $r24, $sp, CTX_REGS + 24*8     # regs[24] = $s1
    st.d        $r25, $sp, CTX_REGS + 25*8     # regs[25] = $s2
    st.d        $r26, $sp, CTX_REGS + 26*8     # regs[26] = $s3
    st.d        $r27, $sp, CTX_REGS + 27*8     # regs[27] = $s4
    st.d        $r28, $sp, CTX_REGS + 28*8     # regs[28] = $s5
    st.d        $r29, $sp, CTX_REGS + 29*8     # regs[29] = $s6
    st.d        $r30, $sp, CTX_REGS + 30*8     # regs[30] = $s7
    st.d        $r31, $sp, CTX_REGS + 31*8     # regs[31] = $s8

    # ==========================================================================
    # 第 4 步：保存关键 CSR 寄存器
    # Step 4: Save critical CSR registers
    #
    # 🟢 复用自 PolyHAL 的 CSR 保存框架（MIT/Apache-2.0）
    # ==========================================================================
    # ERA — 异常返回地址（对应 RISC-V sepc）
    csrrd       $t0, 0x6            # $t0 = ERA
    st.d        $t0, $sp, CTX_ERA   # 保存 ERA

    # PRMD — 先前模式信息（对应 RISC-V sstatus 的 SPP/SPIE）
    csrrd       $t0, 0x1            # $t0 = PRMD
    st.d        $t0, $sp, CTX_PRMD  # 保存 PRMD

    # BADV — 出错虚拟地址（对应 RISC-V stval）
    csrrd       $t0, 0x7            # $t0 = BADV
    st.d        $t0, $sp, CTX_BADV  # 保存 BADV

    # ==========================================================================
    # 第 5 步：保存内核栈指针到 TrapContext
    # Step 5: Save kernel stack pointer to TrapContext
    #
    # kernel_sp 字段用于下次从用户态陷入时恢复内核栈
    # ==========================================================================
    addi.d      $t0, $sp, CTX_SIZE  # $t0 = 当前 $sp + CTX_SIZE = 内核栈顶
    st.d        $t0, $sp, CTX_KERNEL_SP  # 保存 kernel_sp

    # ==========================================================================
    # 第 6 步：调用 Rust trap handler
    # Step 6: Call Rust trap handler
    #
    # 🟡 微内核适配：调用 la64_trap_handler(cx: &mut La64TrapContext)
    # 参数: $a0 = TrapContext 指针（即当前 $sp）
    # 返回: $a0 = TrapContext 指针（可能被调度器修改为其他进程的上下文）
    # ==========================================================================
    move        $a0, $sp            # $a0 = &mut La64TrapContext
    bl          la64_trap_handler   # 调用 Rust trap handler

__la64_commit_and_restore:
    # 先物理切到目标 TrapContext 下方的内核栈，再提交调度状态。临时保存 a0
    # 的 16 字节位于目标栈、TrapContext 之外；Rust finalizer 返回后恢复指针。
    move        $sp, $a0
    addi.d      $sp, $sp, -16
    st.d        $a0, $sp, 0
    bl          finish_staged_user_return
    ld.d        $a0, $sp, 0
    addi.d      $sp, $sp, 16
    move        $sp, $a0

# =============================================================================
# __la64_restore — 从 TrapContext 恢复寄存器并返回
# Restore Registers from TrapContext and Return
#
# 🟢 寄存器恢复框架复用自 PolyHAL（MIT/Apache-2.0）
#
# 参数: $sp 指向 La64TrapContext
# =============================================================================
__la64_restore:
    # ==========================================================================
    # 第 1 步：恢复关键 CSR 寄存器
    # Step 1: Restore critical CSR registers
    # ==========================================================================
    # 恢复 ERA
    ld.d        $t0, $sp, CTX_ERA
    csrwr       $t0, 0x6            # ERA = 保存的值

    # 恢复 PRMD
    ld.d        $t0, $sp, CTX_PRMD
    csrwr       $t0, 0x1            # PRMD = 保存的值

    # 注意: BADV 是只读的（由硬件设置），不需要恢复
    # Note: BADV is read-only (set by hardware), no need to restore

    # ==========================================================================
    # 第 2 步：将内核栈顶保存到 SAVE1，供下次 trap 使用
    # Step 2: Save kernel stack top to SAVE1 for next trap
    # ==========================================================================
    ld.d        $t0, $sp, CTX_KERNEL_SP
    csrwr       $t0, 0x31           # SAVE1 = kernel_sp

    # ==========================================================================
    # 第 3 步：恢复所有通用寄存器
    # Step 3: Restore all general purpose registers
    #
    # 🟢 复用自 PolyHAL 的寄存器恢复框架（MIT/Apache-2.0）
    #
    # 注意恢复顺序：先恢复除 $sp/$t0 外的所有寄存器，
    # 最后恢复 $sp（因为 $sp 当前指向 TrapContext）
    # ==========================================================================
    # $r1 (ra)
    ld.d        $r1,  $sp, CTX_REGS + 1*8

    # $r2 (tp)
    ld.d        $r2,  $sp, CTX_REGS + 2*8

    # $r4-$r11 (a0-a7)
    ld.d        $r4,  $sp, CTX_REGS + 4*8
    ld.d        $r5,  $sp, CTX_REGS + 5*8
    ld.d        $r6,  $sp, CTX_REGS + 6*8
    ld.d        $r7,  $sp, CTX_REGS + 7*8
    ld.d        $r8,  $sp, CTX_REGS + 8*8
    ld.d        $r9,  $sp, CTX_REGS + 9*8
    ld.d        $r10, $sp, CTX_REGS + 10*8
    ld.d        $r11, $sp, CTX_REGS + 11*8

    # $r12-$r20 (t0-t8)
    ld.d        $r12, $sp, CTX_REGS + 12*8
    ld.d        $r13, $sp, CTX_REGS + 13*8
    ld.d        $r14, $sp, CTX_REGS + 14*8
    ld.d        $r15, $sp, CTX_REGS + 15*8
    ld.d        $r16, $sp, CTX_REGS + 16*8
    ld.d        $r17, $sp, CTX_REGS + 17*8
    ld.d        $r18, $sp, CTX_REGS + 18*8
    ld.d        $r19, $sp, CTX_REGS + 19*8
    ld.d        $r20, $sp, CTX_REGS + 20*8

    # $r21 (reserved)
    ld.d        $r21, $sp, CTX_REGS + 21*8

    # $r22 (fp/s9)
    ld.d        $r22, $sp, CTX_REGS + 22*8

    # $r23-$r31 (s0-s8)
    ld.d        $r23, $sp, CTX_REGS + 23*8
    ld.d        $r24, $sp, CTX_REGS + 24*8
    ld.d        $r25, $sp, CTX_REGS + 25*8
    ld.d        $r26, $sp, CTX_REGS + 26*8
    ld.d        $r27, $sp, CTX_REGS + 27*8
    ld.d        $r28, $sp, CTX_REGS + 28*8
    ld.d        $r29, $sp, CTX_REGS + 29*8
    ld.d        $r30, $sp, CTX_REGS + 30*8
    ld.d        $r31, $sp, CTX_REGS + 31*8

    # ==========================================================================
    # 第 4 步：恢复 $sp（用户栈指针）并返回
    # Step 4: Restore $sp (user stack pointer) and return
    #
    # 最后恢复 $sp，因为之前一直用它访问 TrapContext
    # ==========================================================================
    ld.d        $sp,  $sp, CTX_REGS + 3*8      # $sp = 用户 $sp

    # ==========================================================================
    # 第 5 步：异常返回
    # Step 5: Exception return
    #
    # ertn 指令：
    # - 将 PRMD.PPLV 恢复到 CRMD.PLV（恢复特权级）
    # - 将 PRMD.PIE 恢复到 CRMD.IE（恢复中断使能）
    # - 跳转到 ERA（恢复 PC）
    # ==========================================================================
    ertn                            # 异常返回（Exception Return）

# =============================================================================
# 兼容符号定义 — 供上层代码引用
# Compatibility symbol definitions for upper-layer code references
# =============================================================================

.global __trap_entry
.global __alltraps

# __trap_entry 和 __alltraps 是 RISC-V 的 trap 入口符号名
# LoongArch64 使用 EENTRY CSR 设置 trap 入口，不需要这些符号
# 但上层代码引用了它们，所以提供占位定义
__trap_entry:
__alltraps:
    b __la64_alltraps
