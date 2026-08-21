# =============================================================================
# RustMicroOS — LoongArch64 启动入口
# =============================================================================
#
# 两段式启动：
#   .text.entry 段在物理地址 0x0020_0000（DA=1 模式下执行）
#   .text 段在 DMW0 窗口地址 0x9000_0000_xxxx_xxxx（PG=1 模式下执行）
#
# 启动流程：
#   1. QEMU 将内核加载到物理地址 0x0020_0000，entry point = 0x0020_0000
#   2. CPU 在 DA=1 模式下执行 _start
#   3. 配置 DMW0/DMW1 + 临时 DMW2
#   4. 切换到 PG=1
#   5. 跳转到 .text 段的 _start_high（DMW0 窗口高地址）
#   6. 清除 DMW2，设置栈，清零 BSS，跳转到 rust_main
# =============================================================================

# ============================================================================
# 启动入口（物理地址段）
# ============================================================================
.section .text.entry
.globl _start

_start:
    # 配置 CRMD: DA=1, PG=0, PLV=0, IE=0
    li.w        $t0, 0x8
    csrwr       $t0, 0x0

    # 使能 FPU + LSX(SXE) + LASX(ASXE)：EUEN = FPE|SXE|ASXE = 0x7
    # 向量单元必须在启动即全开：QEMU 对 ASXE=0 时的保留向量编码不抛 ASXD 而是死锁，
    # 全开后解码器正常执行合法向量指令、对保留编码抛 INE → SIGILL（对标真机）。
    li.w        $t0, 0x7
    csrwr       $t0, 0x2

    # 配置 DMW0: VSEG=0x9000, MAT=CC, PLV0=1
    li.d        $t0, 0x9000000000000011
    csrwr       $t0, 0x180

    # 配置 DMW1: VSEG=0x8000, MAT=SUC, PLV0=1
    li.d        $t0, 0x8000000000000001
    csrwr       $t0, 0x181

    # 配置临时 DMW2: VSEG=0x0000, MAT=CC, PLV0=1
    li.d        $t0, 0x0000000000000011
    csrwr       $t0, 0x182

    # 清除 DMW3
    csrwr       $zero, 0x183

    # 切换到 PG=1 模式
    csrrd       $t0, 0x0
    li.d        $t1, 0x18
    andn        $t0, $t0, $t1
    ori         $t0, $t0, 0x10
    csrwr       $t0, 0x0

    # 跳转到高地址的 _start_high（.text 段，DMW0 窗口内）
    # la.abs 在 large 代码模型下可以加载 64 位地址
    la.abs      $t0, _start_high
    jr          $t0

# ============================================================================
# AP entry installed in the QEMU mailbox (physical/DA=1 mode)
# ============================================================================
.globl secondary_hart_start
secondary_hart_start:
    # AP enters with CRMD in direct-address mode, like _start.
    # Configure CRMD: DA=1, PG=0, PLV=0, IE=0.
    li.w        $t0, 0x8
    csrwr       $t0, 0x0
    # Enable FPU, LSX, and LASX, matching the boot CPU EUEN setup.
    li.w        $t0, 0x7
    csrwr       $t0, 0x2
    # Configure DMW0 as cached kernel direct map.
    li.d        $t0, 0x9000000000000011
    csrwr       $t0, 0x180
    # Configure DMW1 as uncached device direct map.
    li.d        $t0, 0x8000000000000001
    csrwr       $t0, 0x181
    # Temporarily expose the low physical address space while switching modes.
    li.d        $t0, 0x0000000000000011
    csrwr       $t0, 0x182
    # Disable DMW3, as in the boot CPU path.
    csrwr       $zero, 0x183
    # Switch from DA=1 to PG=1 while preserving the boot CPU CRMD settings.
    csrrd       $t0, 0x0
    li.d        $t1, 0x18
    andn        $t0, $t0, $t1
    ori         $t0, $t0, 0x10
    csrwr       $t0, 0x0
    la.abs      $t0, secondary_hart_start_high
    jr          $t0

# ============================================================================
# 高地址启动代码（DMW0 窗口内）
# ============================================================================
.section .text
.globl _start_high

_start_high:
    # 清除 DMW2（不再需要低地址映射）
    csrwr       $zero, 0x182

    # 设置栈指针（高地址）
    la.abs      $sp, boot_stack_top

    # 清零 BSS 段
    la.abs      $t0, sbss
    la.abs      $t1, ebss
    bgeu        $t0, $t1, .Lbss_done

.Lbss_clear:
    st.d        $zero, $t0, 0
    addi.d      $t0, $t0, 8
    bltu        $t0, $t1, .Lbss_clear

.Lbss_done:
    # 跳转到 Rust 入口
    bl          rust_main

.globl secondary_hart_start_high
secondary_hart_start_high:
    # DMW2 was only a low-physical bootstrap window; disable it before Rust.
    csrwr       $zero, 0x182
    # CPUNUM CSR 0x20 identifies this hart and selects its 64 KiB stack slot.
    csrrd       $a0, 0x20
    andi        $a0, $a0, 0x3ff
    li.w        $t1, 16
    bgeu        $a0, $t1, .Lhalt
    la.abs      $sp, boot_stack_top
    slli.d      $t0, $a0, 16
    sub.d       $sp, $sp, $t0
    bl          secondary_hart_main

.Lhalt:
    idle        0
    b           .Lhalt

# ============================================================================
# 启动栈定义
# ============================================================================
.section .bss.stack
.globl boot_stack
.align 12
boot_stack:
    .space 1048576   # 16 个 hart，每个 64KB（须 >= sched::smp::MAX_CPUS）
.globl boot_stack_top
boot_stack_top:
