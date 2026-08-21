macro_rules! includes_trap_macros {
    () => {
        r#"
        .ifndef REGS_TRAP_MACROS_FLAG
        .equ REGS_TRAP_MACROS_FLAG, 1

        .macro LDR  reg, offset
            ld  \reg, \offset*8(sp)
        .endm

        .macro STR  reg, offset
            sd  \reg, \offset*8(sp)
        .endm

        .macro LOAD reg, offset
            ld  \reg, \offset*8(sp)
        .endm

        .macro SAVE reg, offset
            sd  \reg, \offset*8(sp)
        .endm

        .macro LOAD_N n
            ld  x\n, \n*8(sp)
        .endm

        .macro SAVE_N n
            sd  x\n, \n*8(sp)
        .endm

        .macro SAVE_GENERAL_REGS
            SAVE    x1, 1
            csrr    x1, sscratch
            SAVE    x1, 2
            .set    n, 3
            .rept   29
                SAVE_N  %n
            .set    n, n + 1
            .endr

            csrr    t0, sstatus
            csrr    t1, sepc
            SAVE    t0, 32
            SAVE    t1, 33
        .endm

        .macro LOAD_GENERAL_REGS
            LOAD    t0, 32
            LOAD    t1, 33
            csrw    sstatus, t0
            csrw    sepc, t1

            LOAD    x1, 1
            .set    n, 3
            .rept   29
                LOAD_N  %n
            .set    n, n + 1
            .endr
            LOAD    x2, 2
        .endm

        .macro LOAD_PERCPU dst, sym
            lui  \dst, %hi(__PERCPU_\sym)
            add  \dst, \dst, gp
            ld   \dst, %lo(__PERCPU_\sym)(\dst)
        .endm

        .macro SAVE_PERCPU sym, temp, src
            lui  \temp, %hi(__PERCPU_\sym)
            add  \temp, \temp, gp
            sd   \src,  %lo(__PERCPU_\sym)(\temp)
        .endm

        .macro SAVE_FP_REGS
            .option push
            .option arch, +d
            csrr    t0, fcsr
            sd      t0, 272(sp)
            fsd     f0,  280(sp)
            fsd     f1,  288(sp)
            fsd     f2,  296(sp)
            fsd     f3,  304(sp)
            fsd     f4,  312(sp)
            fsd     f5,  320(sp)
            fsd     f6,  328(sp)
            fsd     f7,  336(sp)
            fsd     f8,  344(sp)
            fsd     f9,  352(sp)
            fsd     f10, 360(sp)
            fsd     f11, 368(sp)
            fsd     f12, 376(sp)
            fsd     f13, 384(sp)
            fsd     f14, 392(sp)
            fsd     f15, 400(sp)
            fsd     f16, 408(sp)
            fsd     f17, 416(sp)
            fsd     f18, 424(sp)
            fsd     f19, 432(sp)
            fsd     f20, 440(sp)
            fsd     f21, 448(sp)
            fsd     f22, 456(sp)
            fsd     f23, 464(sp)
            fsd     f24, 472(sp)
            fsd     f25, 480(sp)
            fsd     f26, 488(sp)
            fsd     f27, 496(sp)
            fsd     f28, 504(sp)
            fsd     f29, 512(sp)
            fsd     f30, 520(sp)
            fsd     f31, 528(sp)
            .option pop
        .endm

        .macro LOAD_FP_REGS
            .option push
            .option arch, +d
            ld      t0, 272(sp)
            csrw    fcsr, t0
            fld     f0,  280(sp)
            fld     f1,  288(sp)
            fld     f2,  296(sp)
            fld     f3,  304(sp)
            fld     f4,  312(sp)
            fld     f5,  320(sp)
            fld     f6,  328(sp)
            fld     f7,  336(sp)
            fld     f8,  344(sp)
            fld     f9,  352(sp)
            fld     f10, 360(sp)
            fld     f11, 368(sp)
            fld     f12, 376(sp)
            fld     f13, 384(sp)
            fld     f14, 392(sp)
            fld     f15, 400(sp)
            fld     f16, 408(sp)
            fld     f17, 416(sp)
            fld     f18, 424(sp)
            fld     f19, 432(sp)
            fld     f20, 440(sp)
            fld     f21, 448(sp)
            fld     f22, 456(sp)
            fld     f23, 464(sp)
            fld     f24, 472(sp)
            fld     f25, 480(sp)
            fld     f26, 488(sp)
            fld     f27, 496(sp)
            fld     f28, 504(sp)
            fld     f29, 512(sp)
            fld     f30, 520(sp)
            fld     f31, 528(sp)
            .option pop
        .endm

        .endif
        "#
    };
}
