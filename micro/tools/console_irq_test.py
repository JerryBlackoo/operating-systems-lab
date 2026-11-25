#!/usr/bin/env python3
import os
import re
import selectors
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
QEMU = os.environ.get("QEMU", "qemu-system-riscv64")
FS_IMAGE = Path(os.environ.get("MICRO_FS_IMAGE", str(ROOT / "build/micro-root.img")))


def main() -> int:
    cmd = [
        QEMU,
        "-machine", "virt",
        "-m", "128M",
        "-smp", "1",
        "-nographic",
        "-monitor", "none",
        "-bios", "default",
        "-drive", f"file={FS_IMAGE},if=none,format=raw,readonly=on,id=d0",
        "-device", "virtio-blk-device,drive=d0",
        "-initrd", str(ROOT / "build/user/initrd.img"),
        "-kernel", str(ROOT / "build/riscv/kernel.elf"),
    ]
    proc = subprocess.Popen(cmd, cwd=ROOT, stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    assert proc.stdin is not None
    assert proc.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(proc.stdout, selectors.EVENT_READ)
    capture = bytearray()
    sent = False
    passed = False
    deadline = time.monotonic() + 10
    try:
        while time.monotonic() < deadline:
            events = selector.select(max(0, deadline - time.monotonic()))
            if not events:
                break
            chunk = os.read(proc.stdout.fileno(), 4096)
            if not chunk:
                break
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()
            capture.extend(chunk)
            if not sent and b"[console] switched from SBI putchar" in capture:
                proc.stdin.write(b"abc\r")
                proc.stdin.flush()
                sent = True
            if (b'[console] line="abc"' in capture and
                    re.search(rb"\[kmain\] idle: heartbeat.*ext_irqs=[1-9]", capture)):
                passed = True
                break
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
    if not passed:
        print("[console-test] FAIL: missing UART IRQ 10, complete line, or ext_irqs increase")
        return 1
    print("[console-test] PASS: irq=10 line=abc ext_irqs increased")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())