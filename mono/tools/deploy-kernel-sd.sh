#!/usr/bin/env bash
# 把 build/arm/kernel.bin 复制到 SD 卡 FAT 分区。
# 默认只做 dry-run；确认目标路径后加 --apply。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${SRC:-$ROOT/build/arm/kernel.bin}"
APPLY=0

if [[ "${1:-}" == "--apply" ]]; then
    APPLY=1
    shift
fi

DEST="${1:-}"
if [[ -z "$DEST" ]]; then
    echo "usage: $0 [--apply] /path/to/SD/FAT" >&2
    exit 2
fi

if [[ ! -f "$SRC" ]]; then
    echo "missing $SRC; run: make ARCH=arm bin" >&2
    exit 1
fi
if [[ ! -d "$DEST" ]]; then
    echo "not a directory: $DEST" >&2
    exit 1
fi

echo "source: $SRC"
sha256sum "$SRC"
echo "target: $DEST/kernel.bin"

if [[ "$APPLY" -eq 0 ]]; then
    echo "dry-run only; re-run with --apply to copy"
    exit 0
fi

cp "$SRC" "$DEST/kernel.bin"
sync
sha256sum "$DEST/kernel.bin"
echo "copied; U-Boot: fatload mmc 0:1 0x00100000 kernel.bin"
