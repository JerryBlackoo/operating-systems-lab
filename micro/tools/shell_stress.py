#!/usr/bin/env python3
"""长会话回归：同一次启动里连续发 N 条 help，验证 IPC 资源不随命令数泄漏。

覆盖的是 M7 之后发现的真实缺陷：异步 `ABI_IPC_CALL` 的 reply 对象创建引用
没有归还时，`IPC_MAX_REPLIES`（16）耗尽后所有后续调用返回 `ABI_NO_MEMORY`，
表现为 shell 打印第一个提示符后每条命令都静默无输出。

用法:
    python3 tools/shell_stress.py [命令数]     # 默认 40
环境:
    QEMU                qemu 可执行文件（默认 qemu-system-riscv64）
    MICRO_STEP_TIMEOUT  单条命令超时秒数（默认 6.0）
"""
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
os.chdir(ROOT)

N = int(sys.argv[1]) if len(sys.argv) > 1 else 40
STEP = float(os.environ.get("MICRO_STEP_TIMEOUT", "6.0"))
QEMU = os.environ.get("QEMU", "qemu-system-riscv64")

for need in ("build/micro-root-run.img", "build/user/initrd.img",
             "build/riscv/kernel.elf"):
    if not (ROOT / need).exists():
        print(f"[stress] missing {need}; run `make fs-image all initrd` first",
              file=sys.stderr)
        sys.exit(2)

cmd = [
    QEMU, "-machine", "virt", "-m", "128M", "-smp", "1", "-nographic",
    "-monitor", "none", "-bios", "default",
    "-drive", "file=build/micro-root-run.img,if=none,format=raw,readonly=on,id=d0",
    "-device", "virtio-blk-device,drive=d0",
    "-initrd", "build/user/initrd.img",
    "-kernel", "build/riscv/kernel.elf",
]
proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.STDOUT, bufsize=0)
buf = b""


def read_until(pattern, timeout):
    """在已累积输出里找 pattern；未命中则继续读串口，直到超时。"""
    global buf
    rx = re.compile(pattern)
    deadline = time.time() + timeout
    while True:
        if rx.search(buf):
            return True
        if time.time() >= deadline:
            return False
        chunk = proc.stdout.read(4096)
        if not chunk:
            return False
        buf += chunk


def fail(msg):
    tail = buf[-1500:].decode("utf-8", "replace")
    print(f"[stress] FAIL: {msg}")
    print(f"[stress] tail:\n{tail}")
    proc.kill()
    sys.exit(1)


if not read_until(rb"micro> ", 30):
    fail("no shell prompt")
buf = b""

for i in range(N):
    proc.stdin.write(b"help\n")
    proc.stdin.flush()
    if not read_until(rb"commands: help cat ps echo poweroff", STEP):
        fail(f"command {i + 1}/{N}: no help output")
    if not read_until(rb"micro> ", STEP):
        fail(f"command {i + 1}/{N}: no next prompt")
    buf = b""

proc.stdin.write(b"poweroff\n")
proc.stdin.flush()
try:
    proc.wait(timeout=15)
except subprocess.TimeoutExpired:
    fail("poweroff did not exit qemu")

print(f"[stress] PASS {N} commands, qemu exit={proc.returncode}")
