#!/usr/bin/env bash
# 用 QEMU (-s -S) 和 GDB 调试内核。串口输出写入 build/qemu-debug.log。
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL="$ROOT/build/kernel.elf"
LOG="$ROOT/build/qemu-debug.log"

if ! command -v qemu-system-riscv64 >/dev/null 2>&1; then
    printf 'error: 找不到 qemu-system-riscv64，请先运行 %s\n' \
        "$ROOT/tools/check-env.sh" >&2
    exit 1
fi

if [ -z "${GDB:-}" ]; then
    for candidate in riscv64-unknown-elf-gdb gdb-multiarch gdb; do
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

make -C "$ROOT"

QEMU_PID=""
cleanup() {
    if [ -n "$QEMU_PID" ] && kill -0 "$QEMU_PID" >/dev/null 2>&1; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

qemu-system-riscv64 \
    -machine virt -m 128M -nographic -bios default \
    -kernel "$KERNEL" -s -S >"$LOG" 2>&1 &
QEMU_PID=$!

printf 'QEMU 已暂停，等待 GDB 连接 127.0.0.1:1234\n' >&2
printf '串口输出: %s\n' "$LOG" >&2

"$GDB" -q "$KERNEL" \
    -ex 'set pagination off' \
    -ex 'target remote 127.0.0.1:1234' \
    "$@"
