#!/usr/bin/env bash
# 用 QEMU (-s -S) 和 GDB 调试内核。
# 用法：ARCH=riscv|arm tools/debug.sh [GDB 参数]
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCH="${ARCH:-riscv}"

case "$ARCH" in
    riscv)
        KERNEL="$ROOT/build/riscv/kernel.elf"
        QEMU_BIN="qemu-system-riscv64"
        QEMU_ARGS=(-machine virt -m 128M -nographic -bios default)
        ;;
    arm)
        KERNEL="$ROOT/build/arm/kernel.elf"
        QEMU_BIN="qemu-system-arm"
        QEMU_ARGS=(-M xilinx-zynq-a9 -m 512M -nographic)
        ;;
    *)
        printf 'usage: ARCH=riscv|arm %s [GDB 参数]\n' "$0" >&2
        exit 2
        ;;
esac

LOG="$ROOT/build/qemu-debug-$ARCH.log"

if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
    printf 'error: 找不到 %s，请先运行 %s\n' \
        "$QEMU_BIN" "$ROOT/tools/check-env.sh" >&2
    exit 1
fi

if [ -z "${GDB:-}" ]; then
    for candidate in gdb-multiarch riscv64-unknown-elf-gdb arm-none-eabi-gdb gdb; do
        if command -v "$candidate" >/dev/null 2>&1; then
            GDB="$candidate"
            break
        fi
    done
fi
if [ -z "${GDB:-}" ]; then
    printf 'error: 找不到 GDB（Ubuntu/Debian: sudo apt install gdb-multiarch）\n' >&2
    exit 1
fi

mkdir -p "$ROOT/build"
make -C "$ROOT" ARCH="$ARCH"

QEMU_PID=""
cleanup() {
    if [ -n "$QEMU_PID" ] && kill -0 "$QEMU_PID" >/dev/null 2>&1; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

"$QEMU_BIN" "${QEMU_ARGS[@]}" \
    -kernel "$KERNEL" -s -S >"$LOG" 2>&1 &
QEMU_PID=$!

printf 'QEMU 已暂停，等待 GDB 连接 127.0.0.1:1234（%s）\n' "$ARCH" >&2
printf '串口输出: %s\n' "$LOG" >&2

"$GDB" -q "$KERNEL" \
    -ex 'set pagination off' \
    -ex 'target remote 127.0.0.1:1234' \
    "$@"
