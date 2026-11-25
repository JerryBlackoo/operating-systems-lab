#!/usr/bin/env bash
# M5 判别：改 motd 哈希必变；200KiB largefile 全量哈希与宿主一致。
set -euo pipefail

MICRO="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$MICRO/.." && pwd)"
BUILD="$MICRO/build"
KERNEL="$BUILD/riscv/kernel.elf"
INITRD="$BUILD/user/initrd.img"
FS_IMAGE="$BUILD/micro-root.img"
QEMU="${QEMU:-qemu-system-riscv64}"

DEFAULT_MOTD_SHA="74bf28d9c7030ae2deecf3cd87c9ba7580a5cc4b152070d3b1492671bc526e18"
LARGE_SHA="d60c9f8a016b8f4ac40ce4dc1813fc70f6bfa21cf47be8bc26ce612413bac594"
ALT_MOTD="microkernel motd v2-changed\n"

log() { printf '%s\n' "$*"; }

run_qemu_grep() {
    local image="$1"
    local pattern="$2"
    cp -f "$image" "$BUILD/micro-root-run.img"
    timeout 55s "$QEMU" -machine virt -m 128M -smp 1 -nographic -monitor none \
        -bios default \
        -drive "file=$BUILD/micro-root-run.img,if=none,format=raw,readonly=on,id=d0" \
        -device virtio-blk-device,drive=d0 \
        -initrd "$INITRD" -kernel "$KERNEL" 2>&1 \
        | strings | rg -e "$pattern" || true
}

log "[m5] build kernel + initrd + default image"
make -s -C "$MICRO" BOOT_MODULES=capability fs-image user initrd >/dev/null
mkout="$(python3 "$MICRO/tools/mkext4image.py" "$FS_IMAGE" 2>&1 | tail -3)"
log "$mkout"

log "[m5] qemu default motd + largefile hashes"
boot="$(run_qemu_grep "$FS_IMAGE" 'largefile|motd sha256=|\[block\] busy discriminate')"
echo "$boot" | tee /tmp/m5-boot-default.txt
echo "$boot" | rg -q "\[fs\] motd sha256=$DEFAULT_MOTD_SHA" || {
    log "[m5] FAIL default motd sha256"
    exit 1
}
echo "$boot" | rg -q "\[fs\] largefile sha256=$LARGE_SHA" || {
    log "[m5] FAIL largefile sha256 (expected full 200KiB)"
    exit 1
}
echo "$boot" | rg -q '\[ext4\] read /largefile: 204800 bytes' || {
    log "[m5] FAIL largefile byte count"
    exit 1
}
echo "$boot" | rg -q '\[block\] busy discriminate ok' || {
    log "[m5] FAIL block busy discriminate"
    exit 1
}

log "[m5] alternate motd image"
ALT_IMG="$BUILD/micro-root-alt.img"
ALT_MOTD_FILE="$(mktemp)"
printf '%s' 'microkernel motd v2-changed
' >"$ALT_MOTD_FILE"
python3 "$MICRO/tools/mkext4image.py" "$ALT_IMG" "$ALT_MOTD_FILE" >/dev/null
rm -f "$ALT_MOTD_FILE"
alt_sha="$(python3 -c "import hashlib; print(hashlib.sha256(b'microkernel motd v2-changed\n').hexdigest())")"
alt_boot="$(run_qemu_grep "$ALT_IMG" '\[fs\] motd sha256=')"
echo "$alt_boot"
echo "$alt_boot" | rg -q "sha256=$alt_sha" || {
    log "[m5] FAIL alternate motd sha256"
    exit 1
}
if echo "$alt_boot" | rg -q "sha256=$DEFAULT_MOTD_SHA"; then
    log "[m5] FAIL motd hash unchanged after image rewrite"
    exit 1
fi

log "[m5] PASS"