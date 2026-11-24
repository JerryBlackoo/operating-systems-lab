#!/usr/bin/env bash
# 一键编译并用 -nographic 启动 QEMU。
# 用法：ARCH=riscv|arm tools/run-qemu.sh [额外 QEMU 参数]
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
        printf 'usage: ARCH=riscv|arm %s [QEMU 参数]\n' "$0" >&2
        exit 2
        ;;
esac

if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
    printf 'error: 找不到 %s，请先运行 %s\n' \
        "$QEMU_BIN" "$ROOT/tools/check-env.sh" >&2
    exit 1
fi

make -C "$ROOT" ARCH="$ARCH"

printf '启动 QEMU（%s）：Ctrl-A 然后 X 退出。\n' "$ARCH" >&2
exec "$QEMU_BIN" "${QEMU_ARGS[@]}" -kernel "$KERNEL" "$@"
