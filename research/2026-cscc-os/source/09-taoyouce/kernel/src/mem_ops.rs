//! word-at-a-time memcpy/memmove/memset/memcmp，覆盖 compiler_builtins 的
//! 逐字节实现（linker 优先选非 weak 符号）。
//!
//! 背景：compiler_builtins 默认 `copy_forward_bytes` 等是 1 字节/迭代（TCG
//! 下 ~4 insn/字节）；opwindow 实测 lat_fs unlink 热路径每 op 花 520 insn
//! 在 memcpy 上。RV64GC/LA64 (QEMU TCG) 都支持不对齐访问，但为稳妥仍按
//! 「dst 对齐 + src/dst 同余走 8B word」实现，不同余退回逐字节。
//!
//! 注意：循环体用 volatile 读写，防止 LLVM loop-idiom 把 word 循环又识别
//! 回 memcpy 调用造成无限递归。

use core::ptr::{read_volatile, write_volatile};

const WORD: usize = core::mem::size_of::<usize>();

#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    copy_forward(dst, src, n);
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dst as usize) < (src as usize) || (dst as usize) >= (src as usize) + n {
        copy_forward(dst, src, n);
    } else {
        copy_backward(dst, src, n);
    }
    dst
}

#[inline]
unsafe fn copy_forward(mut dst: *mut u8, mut src: *const u8, mut n: usize) {
    // 头部：把 dst 推到 word 对齐
    while n > 0 && (dst as usize) % WORD != 0 {
        write_volatile(dst, read_volatile(src));
        dst = dst.add(1);
        src = src.add(1);
        n -= 1;
    }
    // Rust 的 `read_volatile::<usize>` **要求指针对齐**；硬件支持非对齐访问
    // 不会放宽语言层 precondition。原实现只对齐 dst，src 不同余时仍强转
    // `*const usize`，是 UB：debug 直接 panic，release 会静默读坏用户/内核数据
    // （BuildStorm rustc/collect2 随机 SIGSEGV、realloc invalid old size 根因）。
    // src/dst 不同余时保守逐字节；同余时上面的 dst 对齐也同步对齐了 src。
    if (src as usize) % WORD != 0 {
        while n > 0 {
            write_volatile(dst, read_volatile(src));
            dst = dst.add(1);
            src = src.add(1);
            n -= 1;
        }
        return;
    }
    // 8 路展开：TCG 下每迭代的指针增量/计数/分支开销摊薄到 64B。
    // 此处 src/dst 均已满足 usize 对齐。
    while n >= WORD * 8 {
        let w0 = read_volatile(src as *const usize);
        let w1 = read_volatile(src.add(WORD) as *const usize);
        let w2 = read_volatile(src.add(WORD * 2) as *const usize);
        let w3 = read_volatile(src.add(WORD * 3) as *const usize);
        let w4 = read_volatile(src.add(WORD * 4) as *const usize);
        let w5 = read_volatile(src.add(WORD * 5) as *const usize);
        let w6 = read_volatile(src.add(WORD * 6) as *const usize);
        let w7 = read_volatile(src.add(WORD * 7) as *const usize);
        write_volatile(dst as *mut usize, w0);
        write_volatile(dst.add(WORD) as *mut usize, w1);
        write_volatile(dst.add(WORD * 2) as *mut usize, w2);
        write_volatile(dst.add(WORD * 3) as *mut usize, w3);
        write_volatile(dst.add(WORD * 4) as *mut usize, w4);
        write_volatile(dst.add(WORD * 5) as *mut usize, w5);
        write_volatile(dst.add(WORD * 6) as *mut usize, w6);
        write_volatile(dst.add(WORD * 7) as *mut usize, w7);
        dst = dst.add(WORD * 8);
        src = src.add(WORD * 8);
        n -= WORD * 8;
    }
    while n >= WORD {
        write_volatile(dst as *mut usize, read_volatile(src as *const usize));
        dst = dst.add(WORD);
        src = src.add(WORD);
        n -= WORD;
    }
    while n > 0 {
        write_volatile(dst, read_volatile(src));
        dst = dst.add(1);
        src = src.add(1);
        n -= 1;
    }
}

#[inline]
unsafe fn copy_backward(dst: *mut u8, src: *const u8, mut n: usize) {
    // 从尾往头拷（dst 与 src 重叠且 dst 在后）
    while n > 0 && ((dst as usize) + n) % WORD != 0 {
        n -= 1;
        write_volatile(dst.add(n), read_volatile(src.add(n)));
    }
    // 与 forward 同理：只有双方尾地址同余、因而此刻同时对齐时才能做 word 读写。
    if ((src as usize) + n) % WORD != 0 {
        while n > 0 {
            n -= 1;
            write_volatile(dst.add(n), read_volatile(src.add(n)));
        }
        return;
    }
    while n >= WORD {
        n -= WORD;
        let w = read_volatile(src.add(n) as *const usize);
        write_volatile(dst.add(n) as *mut usize, w);
    }
    while n > 0 {
        n -= 1;
        write_volatile(dst.add(n), read_volatile(src.add(n)));
    }
}

#[no_mangle]
pub unsafe extern "C" fn memset(dst: *mut u8, c: i32, mut n: usize) -> *mut u8 {
    let b = c as u8;
    let mut p = dst;
    while n > 0 && (p as usize) % WORD != 0 {
        write_volatile(p, b);
        p = p.add(1);
        n -= 1;
    }
    let pattern = usize::from_ne_bytes([b; WORD]);
    // 8 路展开：64B/迭代摊薄循环开销。
    while n >= WORD * 8 {
        write_volatile(p as *mut usize, pattern);
        write_volatile(p.add(WORD) as *mut usize, pattern);
        write_volatile(p.add(WORD * 2) as *mut usize, pattern);
        write_volatile(p.add(WORD * 3) as *mut usize, pattern);
        write_volatile(p.add(WORD * 4) as *mut usize, pattern);
        write_volatile(p.add(WORD * 5) as *mut usize, pattern);
        write_volatile(p.add(WORD * 6) as *mut usize, pattern);
        write_volatile(p.add(WORD * 7) as *mut usize, pattern);
        p = p.add(WORD * 8);
        n -= WORD * 8;
    }
    while n >= WORD {
        write_volatile(p as *mut usize, pattern);
        p = p.add(WORD);
        n -= WORD;
    }
    while n > 0 {
        write_volatile(p, b);
        p = p.add(1);
        n -= 1;
    }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, mut n: usize) -> i32 {
    let mut pa = a;
    let mut pb = b;
    // 双方同余时 word 比较
    if (pa as usize) % WORD == (pb as usize) % WORD {
        while n > 0 && (pa as usize) % WORD != 0 {
            let (x, y) = (read_volatile(pa), read_volatile(pb));
            if x != y {
                return x as i32 - y as i32;
            }
            pa = pa.add(1);
            pb = pb.add(1);
            n -= 1;
        }
        while n >= WORD {
            let x = read_volatile(pa as *const usize);
            let y = read_volatile(pb as *const usize);
            if x != y {
                break; // 落到字节循环找出第一个不同字节
            }
            pa = pa.add(WORD);
            pb = pb.add(WORD);
            n -= WORD;
        }
    }
    while n > 0 {
        let (x, y) = (read_volatile(pa), read_volatile(pb));
        if x != y {
            return x as i32 - y as i32;
        }
        pa = pa.add(1);
        pb = pb.add(1);
        n -= 1;
    }
    0
}
