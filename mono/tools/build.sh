#!/usr/bin/env bash
# 编译内核。只是 Makefile 的薄封装，不包含任何内核逻辑。
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCH="${1:-riscv}"

case "$ARCH" in
    riscv|arm)
        make -C "$ROOT" ARCH="$ARCH"
        ;;
    clean)
        make -C "$ROOT" clean
        ;;
    *)
        printf 'usage: %s [riscv|arm|clean]\n' "$0" >&2
        exit 2
        ;;
esac
