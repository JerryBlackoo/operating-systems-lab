#!/usr/bin/env bash
# 检查构建和运行环境。只做检查，不编译、不启动 QEMU。
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAILED=0
WARNED=0

ok()   { printf '[ok]   %s\n' "$1"; }
warn() { printf '[warn] %s\n' "$1"; WARNED=$((WARNED + 1)); }
fail() { printf '[fail] %s\n' "$1"; FAILED=$((FAILED + 1)); }

# 与 Makefile 保持一致的链接器查找顺序：ld.lld -> rust-lld -> rustup 工具链。
find_lld() {
    if command -v ld.lld >/dev/null 2>&1; then
        command -v ld.lld
        return 0
    fi
    if command -v rust-lld >/dev/null 2>&1; then
        command -v rust-lld
        return 0
    fi
    local candidate
    for candidate in "$HOME"/.rustup/toolchains/*/lib/rustlib/*/bin/rust-lld; do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

printf '检查 QEMU 与交叉编译工具链（RISC-V / ARM）...\n\n'

# QEMU
if QEMU_BIN="$(command -v qemu-system-riscv64 2>/dev/null)"; then
    QEMU_VER="$(qemu-system-riscv64 --version 2>/dev/null | head -n 1)"
    ok "qemu-system-riscv64: $QEMU_BIN ($QEMU_VER)"
else
    QEMU_BIN=""
    fail "缺少 qemu-system-riscv64（Ubuntu/Debian: sudo apt install qemu-system-misc）"
fi

if QEMU_ARM_BIN="$(command -v qemu-system-arm 2>/dev/null)"; then
    QEMU_ARM_VER="$(qemu-system-arm --version 2>/dev/null | head -n 1)"
    ok "qemu-system-arm: $QEMU_ARM_BIN ($QEMU_ARM_VER)"
else
    fail "缺少 qemu-system-arm（Ubuntu/Debian: sudo apt install qemu-system-arm）"
fi

# clang
if CLANG_BIN="$(command -v clang 2>/dev/null)"; then
    CLANG_VER="$(clang --version 2>/dev/null | head -n 1)"
    ok "clang: $CLANG_BIN ($CLANG_VER)"
else
    CLANG_BIN=""
    fail "缺少 clang"
fi

# LLD
if LLD_BIN="$(find_lld)"; then
    ok "LLD: $LLD_BIN"
else
    fail "缺少 ld.lld / rust-lld（Ubuntu/Debian: sudo apt install lld）"
fi

# llvm-objcopy：用户态 flat 镜像和 ARM kernel.bin 都依赖它
if OBJCOPY_BIN="$(command -v llvm-objcopy 2>/dev/null)"; then
    OBJCOPY_VER="$(llvm-objcopy --version 2>/dev/null | head -n 1)"
    ok "llvm-objcopy: $OBJCOPY_BIN ($OBJCOPY_VER)"
else
    OBJCOPY_BIN=""
    fail "缺少 llvm-objcopy（Ubuntu/Debian: sudo apt install llvm）"
fi

# clang 能否交叉编译 RISC-V 64（与 Makefile 的 CFLAGS 保持一致）
if [ -n "$CLANG_BIN" ]; then
    PROBE_DIR="$(mktemp -d)"
    trap 'rm -rf "$PROBE_DIR"' EXIT
    printf 'int probe;\n' > "$PROBE_DIR/probe.c"
    if clang --target=riscv64-unknown-elf -march=rv64imac_zicsr_zifencei -mabi=lp64 \
             -mcmodel=medany -ffreestanding -nostdlib -c \
             "$PROBE_DIR/probe.c" -o "$PROBE_DIR/probe.o" >/dev/null 2>&1; then
        ok "clang RISC-V 交叉编译: --target=riscv64-unknown-elf 可用"
    else
        fail "clang 无法编译 riscv64-unknown-elf 目标"
    fi

    printf 'int probe;\n' > "$PROBE_DIR/probe-arm.c"
    if clang --target=arm-none-eabi -mcpu=cortex-a9 -march=armv7-a -marm \
             -mfloat-abi=soft -ffreestanding -nostdlib -c \
             "$PROBE_DIR/probe-arm.c" -o "$PROBE_DIR/probe-arm.o" >/dev/null 2>&1; then
        ok "clang ARM 交叉编译: --target=arm-none-eabi 可用"
    else
        fail "clang 无法编译 arm-none-eabi 目标"
    fi
else
    warn "跳过 clang 交叉编译检查（clang 不可用）"
fi

# OpenSBI 固件（-bios default 使用，缺失时 QEMU 启动会报错）
BIOS=""
for candidate in \
    /usr/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin \
    /usr/local/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin; do
    if [ -f "$candidate" ]; then
        BIOS="$candidate"
        break
    fi
done
if [ -z "$BIOS" ] && [ -n "$QEMU_BIN" ]; then
    QEMU_PREFIX="$(cd "$(dirname "$QEMU_BIN")/.." 2>/dev/null && pwd)"
    candidate="$QEMU_PREFIX/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin"
    [ -f "$candidate" ] && BIOS="$candidate"
fi
if [ -n "$BIOS" ]; then
    ok "OpenSBI 固件: $BIOS"
else
    warn "未在常见路径找到 OpenSBI 固件，-bios default 可能仍然可用"
fi

# GDB（仅调试需要）
if command -v riscv64-unknown-elf-gdb >/dev/null 2>&1; then
    ok "GDB: $(command -v riscv64-unknown-elf-gdb)"
elif command -v gdb-multiarch >/dev/null 2>&1; then
    ok "GDB: $(command -v gdb-multiarch)"
elif command -v gdb >/dev/null 2>&1; then
    warn "只找到 gdb，未必支持 RISC-V；建议安装 gdb-multiarch"
else
    warn "未找到 GDB，tools/debug.sh 不可用（Ubuntu/Debian: sudo apt install gdb-multiarch）"
fi

# GNU 交叉工具链不是构建所需，仅作信息展示。
if command -v riscv64-unknown-elf-gcc >/dev/null 2>&1; then
    ok "GNU RISC-V 工具链（可选）: $(command -v riscv64-unknown-elf-gcc)"
else
    printf '[info] 未安装 GNU RISC-V 交叉工具链（当前构建使用 clang/LLD，不需要）\n'
fi

printf '\n'
if [ "$FAILED" -ne 0 ]; then
    printf '环境检查失败：%d 项必须依赖缺失。\n' "$FAILED"
    exit 1
fi
if [ "$WARNED" -ne 0 ]; then
    printf '环境检查通过（%d 项警告，不影响构建）。\n' "$WARNED"
else
    printf '环境检查通过。\n'
fi
