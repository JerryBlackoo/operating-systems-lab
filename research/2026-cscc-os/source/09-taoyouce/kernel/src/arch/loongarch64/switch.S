# =============================================================================
# LoongArch64 内核上下文切换汇编 / Kernel Context Switch Assembly
#
# 来源说明 / Source Attribution:
#   Callee-saved 寄存器保存/恢复框架参考自 PolyHAL 项目
#   (https://github.com/Byte-OS/polyhal) 的 Context 切换实现，
#   适配 RustMicroOS 微内核架构。
#   遵循 MIT/Apache-2.0 双协议。
#
#   Callee-saved register save/restore framework adapted from PolyHAL project's
#   Context switch implementation, customized for RustMicroOS microkernel.
#   Licensed under MIT/Apache-2.0 dual license.
#
# 函数签名 / Function Signature:
#   extern "C" fn __la64_switch(
#       current: *mut La64Context,  // $a0 ($r4)
#       next: *const La64Context,   // $a1 ($r5)
#   )
#
# La64Context 内存布局 / Memory Layout:
#   偏移 0x00: ra   ($r1)   — 返回地址 / Return address
#   偏移 0x08: sp   ($r3)   — 栈指针 / Stack pointer
#   偏移 0x10: s0   ($r23)  — Callee-saved
#   偏移 0x18: s1   ($r24)  — Callee-saved
#   偏移 0x20: s2   ($r25)  — Callee-saved
#   偏移 0x28: s3   ($r26)  — Callee-saved
#   偏移 0x30: s4   ($r27)  — Callee-saved
#   偏移 0x38: s5   ($r28)  — Callee-saved
#   偏移 0x40: s6   ($r29)  — Callee-saved
#   偏移 0x48: s7   ($r30)  — Callee-saved
#   偏移 0x50: s8   ($r31)  — Callee-saved
#   偏移 0x58: fp   ($r22)  — 帧指针 / Frame pointer
#   偏移 0x60: tp   ($r2)   — 线程指针 / Thread pointer
#
#   总大小: 13 * 8 = 104 字节 = 0x68
#
# LoongArch64 寄存器编号参考 / Register Number Reference:
#   $r0  = $zero    $r1  = $ra     $r2  = $tp     $r3  = $sp
#   $r4  = $a0      $r5  = $a1     $r22 = $fp     $r23 = $s0
#   $r24 = $s1      $r25 = $s2     $r26 = $s3     $r27 = $s4
#   $r28 = $s5      $r29 = $s6     $r30 = $s7     $r31 = $s8
# =============================================================================

    .section .text
    .globl __la64_switch
    .type __la64_switch, @function
    .balign 16

__la64_switch:
    # =========================================================================
    # 保存当前上下文的 callee-saved 寄存器到 current ($a0)
    # Save current context's callee-saved registers to current ($a0)
    # =========================================================================

    st.d    $ra,  $a0, 0x00        # current.ra  = $ra  ($r1)
    st.d    $sp,  $a0, 0x08        # current.sp  = $sp  ($r3)
    st.d    $s0,  $a0, 0x10        # current.s0  = $s0  ($r23)
    st.d    $s1,  $a0, 0x18        # current.s1  = $s1  ($r24)
    st.d    $s2,  $a0, 0x20        # current.s2  = $s2  ($r25)
    st.d    $s3,  $a0, 0x28        # current.s3  = $s3  ($r26)
    st.d    $s4,  $a0, 0x30        # current.s4  = $s4  ($r27)
    st.d    $s5,  $a0, 0x38        # current.s5  = $s5  ($r28)
    st.d    $s6,  $a0, 0x40        # current.s6  = $s6  ($r29)
    st.d    $s7,  $a0, 0x48        # current.s7  = $s7  ($r30)
    st.d    $s8,  $a0, 0x50        # current.s8  = $s8  ($r31)
    st.d    $fp,  $a0, 0x58        # current.fp  = $fp  ($r22)
    st.d    $tp,  $a0, 0x60        # current.tp  = $tp  ($r2)

    # =========================================================================
    # 恢复目标上下文的 callee-saved 寄存器从 next ($a1)
    # Restore next context's callee-saved registers from next ($a1)
    # =========================================================================

    ld.d    $ra,  $a1, 0x00        # $ra  ($r1)  = next.ra
    ld.d    $sp,  $a1, 0x08        # $sp  ($r3)  = next.sp
    ld.d    $s0,  $a1, 0x10        # $s0  ($r23) = next.s0
    ld.d    $s1,  $a1, 0x18        # $s1  ($r24) = next.s1
    ld.d    $s2,  $a1, 0x20        # $s2  ($r25) = next.s2
    ld.d    $s3,  $a1, 0x28        # $s3  ($r26) = next.s3
    ld.d    $s4,  $a1, 0x30        # $s4  ($r27) = next.s4
    ld.d    $s5,  $a1, 0x38        # $s5  ($r28) = next.s5
    ld.d    $s6,  $a1, 0x40        # $s6  ($r29) = next.s6
    ld.d    $s7,  $a1, 0x48        # $s7  ($r30) = next.s7
    ld.d    $s8,  $a1, 0x50        # $s8  ($r31) = next.s8
    ld.d    $fp,  $a1, 0x58        # $fp  ($r22) = next.fp
    ld.d    $tp,  $a1, 0x60        # $tp  ($r2)  = next.tp

    # =========================================================================
    # 跳转到目标上下文的返回地址
    # Jump to next context's return address
    #
    # jr $ra 等价于 jirl $zero, $ra, 0
    # 恢复的 $ra 指向目标线程上次被切换出去时的返回地址，
    # 执行 jr $ra 后将从该地址继续执行。
    # =========================================================================

    jr      $ra

    .size __la64_switch, . - __la64_switch
