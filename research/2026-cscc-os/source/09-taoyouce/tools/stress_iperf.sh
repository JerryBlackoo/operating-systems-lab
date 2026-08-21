#!/bin/bash
# Stress test: run iperf-musl N times without trace, record pass/fail
set -e

RUNS=${1:-5}
PASS=0
FAIL=0
TIMEOUT=180

for i in $(seq 1 $RUNS); do
    echo "=== Run $i/$RUNS ==="
    OUTPUT=$(timeout $TIMEOUT qemu-system-riscv64 \
        -machine virt -kernel kernel-rv -m 512M -nographic -smp 2 \
        -bios default \
        -drive file=testdata/sdcard-rv.img,if=none,format=raw,id=x0 \
        -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
        -no-reboot \
        -device virtio-net-device,netdev=net \
        -netdev user,id=net,hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555 \
        -rtc base=utc \
        -append "SLICE_GROUPS=iperf-musl" 2>&1 || true)
    
    if echo "$OUTPUT" | grep -q '\[ILL\]'; then
        echo "  RESULT: FAIL ([ILL] detected)"
        FAIL=$((FAIL+1))
        # Save the failing output
        echo "$OUTPUT" > "/workspace/iperf_fail_run${i}.log"
        # Extract ILL line
        echo "$OUTPUT" | grep '\[ILL\]'
    elif echo "$OUTPUT" | grep -q 'REVERSE_UDP end: success'; then
        echo "  RESULT: PASS (all 4 tests passed)"
        PASS=$((PASS+1))
    elif echo "$OUTPUT" | grep -q 'panic\|PANIC'; then
        echo "  RESULT: PANIC"
        FAIL=$((FAIL+1))
        echo "$OUTPUT" > "/workspace/iperf_panic_run${i}.log"
        echo "$OUTPUT" | grep -i 'panic'
    else
        echo "  RESULT: TIMEOUT/HANG"
        FAIL=$((FAIL+1))
        echo "$OUTPUT" > "/workspace/iperf_hang_run${i}.log"
        echo "  Last 5 lines:"
        echo "$OUTPUT" | tail -5
    fi
    echo ""
done

echo "==============================="
echo "SUMMARY: $PASS/$RUNS passed, $FAIL/$RUNS failed"
echo "==============================="
