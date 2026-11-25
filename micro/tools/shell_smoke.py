#!/usr/bin/env python3
"""交互烟雾测试：逐条执行 shell 命令，校验完整输出（含输出未被交错的检查）。"""
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path("/home/jerry/Projects/from-scratch/operating-systems-lab/micro")
os.chdir(ROOT)

cmd = [
    "qemu-system-riscv64", "-machine", "virt", "-m", "128M", "-smp", "1",
    "-nographic", "-monitor", "none", "-bios", "default",
    "-drive", "file=build/micro-root-run.img,if=none,format=raw,readonly=on,id=d0",
    "-device", "virtio-blk-device,drive=d0",
    "-initrd", "build/user/initrd.img",
    "-kernel", "build/riscv/kernel.elf",
]
proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.STDOUT, bufsize=0)
buf = bytearray()


def wait_for(pat, timeout):
    rx = re.compile(pat)
    end = time.time() + timeout
    while time.time() < end:
        if rx.search(bytes(buf)):
            return True
        chunk = proc.stdout.read(4096)
        if not chunk:
            return False
        buf.extend(chunk)
    return False


def run(text, expect, timeout=15):
    buf.clear()
    proc.stdin.write(text.encode() + b"\n")
    proc.stdin.flush()
    ok = wait_for(expect.encode(), timeout)
    body = bytes(buf)
    # 交错探测：help 的整行必须连续出现
    return ok, body


if not wait_for(rb"micro> ", 40):
    print("BOOT FAIL")
    proc.kill()
    sys.exit(1)

checks = [
    ("help", "commands: help cat ps echo poweroff"),
    ("cat /motd", "microkernel motd v1"),
    ("cat /motd", "----- end cat -----"),
    ("cat /nonexistent", "not found"),
    ("echo hello-world-42", "hello-world-42"),
    ("ps", "ps:"),
]

fails = []
for input_line, expect in checks:
    ok, body = run(input_line, expect)
    status = "ok" if ok else "FAIL"
    if not ok:
        fails.append((input_line, expect))
    print(f"  {status:4} {input_line!r} -> {expect!r}")

# help 的整行必须未被 fs 启动日志打断（这是之前的间歇性缺陷）
buf.clear()
proc.stdin.write(b"help\n")
proc.stdin.flush()
wait_for(rb"commands: help cat ps echo poweroff", 10)
interleaved = re.search(rb"commands:[^\n]*\[(fs|ext4|vm|block)\]", bytes(buf))
print(f"  {'FAIL' if interleaved else 'ok  ':4} help 行未被其它服务日志交错")
if interleaved:
    fails.append(("help", "interleaving"))

proc.stdin.write(b"poweroff\n")
proc.stdin.flush()
try:
    proc.wait(timeout=15)
except subprocess.TimeoutExpired:
    fails.append(("poweroff", "timeout"))
    proc.kill()

print("SMOKE PASS" if not fails else f"SMOKE FAIL {fails}")
sys.exit(0 if not fails else 1)
