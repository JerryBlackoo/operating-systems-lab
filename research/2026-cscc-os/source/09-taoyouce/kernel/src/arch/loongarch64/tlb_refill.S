# LoongArch64 TLB 重填异常入口
# 使用软件方式遍历页表并填充 TLB
#
# 注意：TLB 重填异常在特殊模式下执行（CRMD.DA=1 临时恢复），
# 但 LoongArch64 的 TLB 重填异常处理有特殊规则：
# - 进入 TLB 重填异常时，CRMD 被保存到 TLBRPRMD
# - CRMD.DA 被设置为 1（直接地址翻译）
# - CRMD.PG 被设置为 0
# - 因此在 TLB 重填处理中，物理地址可以直接访问
# - ertn 返回时恢复 CRMD（PG=1, DA=0）

    .section .text
    .globl __la64_tlb_refill
    .align 12

__la64_tlb_refill:
    # 保存 $t0, $t1 到 TLBRSAVE 和 SAVE0
    csrwr       $t0, 0x8B           # TLBRSAVE = $t0
    csrwr       $t1, 0x30           # SAVE0 = $t1

    # 读取 PGD（硬件自动选择 PGDL/PGDH）
    csrrd       $t0, 0x1B           # $t0 = PGD

    # === LDDIR 硬件辅助走表（PWCL/PWCH 已在 init_tlb 配置）===
    # 目录项 = 裸物理指针（new_directory/new_table 不带标志位），
    # 空目录项 = 0 → lddir 直接返回后可用 beqz 判失败。
    # level 3 = DIR3(PGD, base 30)，level 1 = DIR1(PMD, base 21)。
    lddir       $t0, $t0, 3         # $t0 = PGD[idx]（PMD 基地址）
    beqz        $t0, .Lrefill_fail
    lddir       $t0, $t0, 1         # $t0 = PMD[idx]（PTE 基地址）
    beqz        $t0, .Lrefill_fail

    # === 第三级：读取 PTE 对（偶数页 + 奇数页）===
    # 偶数对索引 = BADV[20:13]，每对 16 字节
    csrrd       $t1, 0x89
    bstrpick.d  $t1, $t1, 20, 13
    alsl.d      $t0, $t1, $t0, 4    # $t0 = &PTE[even_index]

    # 故障侧 PTE 无效（V=0，含空 PTE/swap entry）→ 不 tlbfill，直接走慢路径。
    # 否则会把 V=0 条目填进 TLB：重执行触发 PIL/PIS，装页后必须 invtlb 单页
    # ——QEMU ≤10.x 把它实现为整个 softmmu TLB flush，使每次按需调页（malloc
    # 堆增长、CoW 首触）都付出全 TLB 重填成本。不填即无残留，装页后重执行
    # 走 refill 装载有效 PTE，全程零 invtlb。
    csrrd       $t1, 0x89
    bstrpick.d  $t1, $t1, 12, 12    # BADV bit12 = 奇偶
    alsl.d      $t1, $t1, $t0, 3    # $t1 = &PTE[fault]
    ld.d        $t1, $t1, 0
    andi        $t1, $t1, 1         # PTE_V
    beqz        $t1, .Lrefill_fail

    # 读取偶数页 PTE → TLBRELO0
    ld.d        $t1, $t0, 0         # $t1 = PTE[even]
    csrwr       $t1, 0x8C           # TLBRELO0 = PTE[even]

    # 读取奇数页 PTE → TLBRELO1
    ld.d        $t1, $t0, 8         # $t1 = PTE[odd]
    csrwr       $t1, 0x8D           # TLBRELO1 = PTE[odd]

    # TLBREHI 免写：进入 TLBR 异常时硬件已把 BADV 的 VPPN 写入
    # TLBREHI.VPPN（QEMU raise_mmu_exception 实锤，FIELD_DP64 只改
    # VPPN、保留 PS）；PS=12 由 init_tlb 启动时预设一次。

    # 填充 TLB
    tlbfill

    # 恢复寄存器并返回
    csrrd       $t1, 0x30           # $t1 = SAVE0
    csrrd       $t0, 0x8B           # $t0 = TLBRSAVE
    ertn

.Lrefill_fail:
    # 页表中没有映射，转入通用 trap 入口
    # 由 Rust handler 触发懒分配或 SIGSEGV
    addi.d      $t0, $zero, 1
    csrwr       $t0, 0x33
    csrrd       $t0, 0x8A           # TLBRERA
    addi.d      $t1, $zero, -2
    and         $t0, $t0, $t1
    csrwr       $t0, 0x6            # ERA
    csrrd       $t0, 0x8F           # TLBRPRMD
    csrwr       $t0, 0x1            # PRMD
    # 彻底退出 TLBR 异常语境，让慢路径按普通异常语义运行/返回：
    #   1) 清 TLBRERA.IsTLBR（bit0）——否则 handler 走不归路径（如
    #      exit_group 终止进程后调度下一任务）时，下一个 usertrapret 的
    #      ertn 仍被硬件按 TLBR 语义解释，错用 TLBRERA/TLBRPRMD 返回到
    #      早已失效的现场（垃圾 sp → 异常入口存栈自陷 → 死机）。
    #   2) CRMD 恢复映射翻译（PG=1, DA=0）——内核经 DMW 窗口不受影响，
    #      而普通 ertn 不会替我们恢复 PG/DA。
    # ERA/PRMD 已从 TLBR 寄存器拷贝，普通 ertn 即可正确重执行出错指令。
    csrrd       $t0, 0x8A
    and         $t0, $t0, $t1       # $t1 仍为 -2：清 IsTLBR
    csrwr       $t0, 0x8A
    addi.d      $t1, $zero, 0x10    # PG=1, DA=0
    addi.d      $t0, $zero, 0x18    # mask: CRMD.DA|PG (bit3|bit4)
    csrxchg     $t1, $t0, 0x0
    csrrd       $t1, 0x30           # restore original $t1
    csrrd       $t0, 0x8B           # restore original $t0
    b           __la64_alltraps
