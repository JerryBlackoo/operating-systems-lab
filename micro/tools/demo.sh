#!/usr/bin/env python3
"""M7 一条命令验收：构建 -> 启动 QEMU -> 真实 stdin 喂命令 -> 断言。

正向：cat /motd 的 SHA 必须与 build/micro-motd.txt 一致。
负向（--expect-nonzero）：cat /nonexistent 必须得到 not found，且不能把
不存在的文件当成 motd 成功。
"""
import hashlib
import os
import re
import selectors
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
os.chdir(ROOT)

CAT_PATH = os.environ.get("MICRO_DEMO_CAT_PATH", "/motd")
NEGATIVE = False
args = sys.argv[1:]
i = 0
while i < len(args):
    a = args[i]
    if a == "--cat-path" and i + 1 < len(args):
        CAT_PATH = args[i + 1]
        i += 2
    elif a.startswith("--cat-path="):
        CAT_PATH = a.split("=", 1)[1]
        i += 1
    elif a == "--expect-nonzero":
        CAT_PATH = "/nonexistent"
        NEGATIVE = True
        i += 1
    else:
        print(f"[demo] usage: {sys.argv[0]} [--cat-path <path> | --expect-nonzero]",
              file=sys.stderr)
        sys.exit(2)

ARTIFACTS = ROOT / ".artifacts"
ARTIFACTS.mkdir(exist_ok=True)
LOG = ARTIFACTS / f"demo-{time.strftime('%Y%m%d-%H%M%S')}.log"


def run_make(*targets):
    cmd = ["make", "-s", *targets]
    r = subprocess.run(cmd, cwd=ROOT)
    if r.returncode != 0:
        print(f"[demo] build failed: {' '.join(cmd)}")
        sys.exit(r.returncode)


print("[demo] build kernel + user + initrd + fs-image")
run_make("fs-image")
run_make("all")
run_make("initrd")
(ROOT / "build" / "micro-root-run.img").write_bytes(
    (ROOT / "build" / "micro-root.img").read_bytes()
)
EXPECTED_SHA = hashlib.sha256(
    (ROOT / "build" / "micro-motd.txt").read_bytes()).hexdigest()
print(f"[demo] expected motd sha256={EXPECTED_SHA}")

QEMU = os.environ.get("QEMU", "qemu-system-riscv64")
boot_timeout = float(os.environ.get("MICRO_BOOT_TIMEOUT", "30"))
step_timeout = float(os.environ.get("MICRO_STEP_TIMEOUT", "20"))

cmd = [
    QEMU, "-machine", "virt", "-m", "128M", "-smp", "1", "-nographic",
    "-monitor", "none", "-bios", "default",
    "-drive", "file=build/micro-root-run.img,if=none,format=raw,readonly=on,id=d0",
    "-device", "virtio-blk-device,drive=d0",
    "-initrd", "build/user/initrd.img",
    "-kernel", "build/riscv/kernel.elf",
]

proc = subprocess.Popen(cmd, cwd=ROOT, stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
assert proc.stdin is not None and proc.stdout is not None
sel = selectors.DefaultSelector()
sel.register(proc.stdout, selectors.EVENT_READ)
capture = bytearray()
log_file = open(LOG, "wb")
failed = []


def pump(timeout):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        events = sel.select(max(0.0, end - time.monotonic()))
        if not events:
            break
        chunk = os.read(proc.stdout.fileno(), 4096)
        if not chunk:
            break
        capture.extend(chunk)
        log_file.write(chunk)
        log_file.flush()


def wait_for(token, timeout, start=0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if token in capture[start:]:
            return True
        events = sel.select(max(0.0, end - time.monotonic()))
        if not events:
            break
        chunk = os.read(proc.stdout.fileno(), 4096)
        if not chunk:
            break
        capture.extend(chunk)
        log_file.write(chunk)
        log_file.flush()
    return token in capture[start:]


def send(line):
    assert proc.stdin is not None
    proc.stdin.write((line + "\r").encode())
    proc.stdin.flush()


def text_since(start):
    return capture[start:].decode("utf-8", "replace")


def strip_console_lines(blob):
    return "".join(line for line in blob.splitlines(keepends=True)
                   if not line.startswith("[console]"))


def fail(name, why):
    failed.append(f"{name}: {why}")
    print(f"[demo] {name} FAIL ({why})")


try:
    if not wait_for(b"micro> ", boot_timeout, 0):
        fail("boot", "shell prompt not seen")
    else:
        print("[demo] shell prompt ok")

    start = len(capture)
    send("help")
    if wait_for(b"micro> ", step_timeout, start):
        seg = text_since(start).replace("\r", "")
        if "commands: help cat ps echo poweroff" in seg:
            print("[demo] help ok")
        else:
            fail("help", "missing command list")
    else:
        fail("help", "no next prompt")

    start = len(capture)
    send(f"cat {CAT_PATH}")
    if wait_for(b"micro> ", step_timeout, start):
        seg = text_since(start).replace("\r", "")
        begin = f"----- cat {CAT_PATH} -----\n"
        end = "----- end cat -----\n"
        if begin in seg and end in seg:
            body = strip_console_lines(seg.split(begin, 1)[1].split(end, 1)[0])
            actual = hashlib.sha256(body.encode()).hexdigest()
            if NEGATIVE:
                if f"cat: {CAT_PATH}: not found" in seg and actual != EXPECTED_SHA:
                    print("[demo] negative mode ok (ENOENT; motd not served)")
                else:
                    fail("cat", f"expected ENOENT, actual sha256={actual}")
            elif actual == EXPECTED_SHA:
                print(f"[demo] cat motd ok (sha256={actual})")
            else:
                fail("cat", f"sha256 mismatch actual={actual} expected={EXPECTED_SHA}")
        else:
            fail("cat", "missing cat markers")
    else:
        fail("cat", "no next prompt")

    start = len(capture)
    send("ps")
    if wait_for(b"micro> ", step_timeout, start):
        seg = text_since(start).replace("\r", "")
        names = ["init", "sm", "pm", "fs", "console", "shell"]
        missing = [n for n in names if f" {n} " not in seg]
        total_m = re.search(r"^total (\d+)$", seg, re.MULTILINE)
        total = int(total_m.group(1)) if total_m else -1
        if not missing and total >= len(names):
            print(f"[demo] ps ok (total {total})")
        else:
            fail("ps", f"missing={missing or '-'} total={total}")
    else:
        fail("ps", "no next prompt")

    start = len(capture)
    send("echo hello")
    if wait_for(b"micro> ", step_timeout, start):
        seg = text_since(start).replace("\r", "")
        if any(line == "hello" for line in seg.splitlines()):
            print("[demo] echo ok")
        else:
            fail("echo", "missing hello line")
    else:
        fail("echo", "no next prompt")

    start = len(capture)
    send("poweroff")
    try:
        proc.wait(timeout=step_timeout)
    except subprocess.TimeoutExpired:
        fail("poweroff", "qemu did not exit")
    pump(2)
    seg = text_since(start).replace("\r", "")
    if proc.returncode == 0 and "[kmain] shutdown requested by shell" in seg:
        print("[demo] poweroff ok")
    else:
        fail("poweroff", f"rc={proc.returncode}")
finally:
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    log_file.close()
    print(f"[demo] log: {LOG}")

if failed:
    print("[demo] FAIL")
    sys.exit(1)
if NEGATIVE:
    print("[demo] negative scenario confirmed (ENOENT did not serve motd); exit 1")
    sys.exit(1)
print("[demo] PASS")
sys.exit(0)