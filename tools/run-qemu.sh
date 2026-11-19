#!/usr/bin/env bash
# 一键编译并用 -nographic 启动 QEMU。额外参数原样传给 qemu-system-riscv64。
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL="$ROOT/build/kernel.elf"

if ! command -v qemu-system-riscv64 >/dev/null 2>&1; then
    printf 'error: 找不到 qemu-system-riscv64，请先运行 %s\n' \
        "$ROOT/tools/check-env.sh" >&2
    exit 1
fi

make -C "$ROOT"

printf '启动 QEMU：Ctrl-A 然后 X 退出。\n' >&2
exec qemu-system-riscv64 \
    -machine virt -m 128M -nographic -bios default \
    -kernel "$KERNEL" "$@"
