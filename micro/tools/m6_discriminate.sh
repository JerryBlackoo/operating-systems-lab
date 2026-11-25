#!/usr/bin/env bash
# M6 判别：两进程同一 VA 落到不同 frame；越权写被 VM 明确拒绝。
set -euo pipefail

MICRO="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$MICRO/build"
QEMU="${QEMU:-qemu-system-riscv64}"
LOG="$MICRO/.artifacts/m6-discriminate-$(date +%Y%m%d-%H%M%S).log"

make -s -C "$MICRO" BOOT_MODULES=capability fs-image user initrd >/dev/null
make -s -C "$MICRO" BOOT_MODULES=capability >/dev/null
cp -f "$BUILD/micro-root.img" "$BUILD/micro-root-run.img"

mkdir -p "$MICRO/.artifacts"
log="$(timeout 55s "$QEMU" -machine virt -m 128M -smp 1 -nographic -monitor none \
    -bios default \
    -drive "file=$BUILD/micro-root-run.img,if=none,format=raw,readonly=on,id=d0" \
    -device virtio-blk-device,drive=d0 \
    -initrd "$BUILD/user/initrd.img" \
    -kernel "$BUILD/riscv/kernel.elf" 2>&1 | strings || true)"
printf '%s\n' "$log" > "$LOG"

fail() {
    echo "[m6] FAIL $*"
    exit 1
}

echo "$log" | rg -q '\[vm\] serving in U-mode as=[0-9a-f]+' ||
    fail "VM not serving in U-mode"
echo "$log" | rg -q '\[vm\] reserve tid=[0-9]+ va=0000000040020000 len=4096 ok' ||
    fail "missing explicit reserve"
echo "$log" | rg -q '\[fault\] resumed' ||
    fail "missing resumed"

echo "$log" | rg '\[vm\] map tid=' | tee /tmp/m6-vm-maps.txt || true
count="$(echo "$log" | rg -c '\[heaptest(2)?\] store ok' || true)"
lines="$(echo "$log" | rg -c '\[vm\] map tid=' || true)"
if [ "${count:-0}" -lt 2 ]; then
    fail "expected 2x heaptest store ok got ${count:-0}"
fi
if [ "${lines:-0}" -lt 2 ]; then
    fail "expected >=2 map lines got ${lines:-0}"
fi

va1="$(rg -o 'va=[0-9a-f]+' /tmp/m6-vm-maps.txt | head -1)"
va2="$(rg -o 'va=[0-9a-f]+' /tmp/m6-vm-maps.txt | tail -1)"
f1="$(rg -o 'frame=[0-9a-f]+' /tmp/m6-vm-maps.txt | head -1)"
f2="$(rg -o 'frame=[0-9a-f]+' /tmp/m6-vm-maps.txt | tail -1)"

[ "$va1" = "$va2" ] || fail "va mismatch $va1 vs $va2"
[ "$f1" != "$f2" ] || fail "same frame for two processes $f1"
echo "$log" | rg -q '\[fault\] rejected tid=[0-9]+ status=-4' ||
    fail "missing explicit deny"
echo "$log" | rg -q '\[vm\] deny tid=[0-9]+ va=0000000050000000 reason=rights' ||
    fail "missing VM rights denial"
if echo "$log" | rg -q 'unexpected trap|\[panic\]'; then
    fail "kernel fell into trap/panic"
fi

echo "[m6] PASS same_va=$va1 frames $f1 vs $f2 (deny status=-4, log=$LOG)"