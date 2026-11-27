#!/usr/bin/env python3
"""ex/xiji 一键验收：宿主算法 + QEMU 真实硬件轨。

检查三类证据：
  1. host：七个实验的算法断言（gcc）；
  2. qemu：BIOS 引导 -> 保护模式 -> 真实设备自检 + 同一套算法；
  3. 回归：输出里必须出现的硬件证据行（缺一即失败）。
"""
from __future__ import annotations

import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
QEMU_DIR = ROOT / "qemu"
TRACE = ROOT / "build" / "traces" / "qemu.stdout"

# QEMU 硬件轨必须出现的证据（对应 hw_verify.c 的每一项）
REQUIRED_HW = [
    "hw-lab1: CMOS+debugcon+isa-debug-exit",
    "hw-exp01-02: 8254+PIC ticks=",
    "hw-exp03: physical partitions a/b/c non-overlapping",
    "hw-exp04: segment-relative physical addressing",
    "hw-exp05: #PF faults=",
    "hw-exp06: IDE identify model=",
    "hw-exp07: tree persisted to LBA",
    "PASS hw-all-qemu-devices",
]

REQUIRED_ALGO = [
    "PASS exp01-process-management",
    "PASS exp02-process-scheduling",
    "PASS exp03-partition-memory",
    "PASS exp04-segment-memory",
    "PASS exp05-demand-paging",
    "PASS exp06-device-management",
    "PASS exp07-file-management",
    "ALL PASS (qemu+hw+algo)",
]

REQUIRED_BOOT = [
    "stage1: hello from MBR",
    "xiji-qemu: real devices",
]


def run(cmd: list[str], cwd: Path, timeout: int) -> subprocess.CompletedProcess:
    return subprocess.run(
        cmd, cwd=cwd, capture_output=True, text=True, check=False, timeout=timeout
    )


def main() -> int:
    if not shutil.which("qemu-system-i386"):
        print("SKIP: qemu-system-i386 不在 PATH 中", file=sys.stderr)
        return 2

    print("== 1/2 host algorithms ==")
    host = run(["make", "test"], ROOT, 120)
    sys.stdout.write(host.stdout)
    sys.stderr.write(host.stderr)
    if host.returncode != 0:
        print("FAIL: make test", file=sys.stderr)
        return 1

    print("== 2/2 QEMU hardware track (TCG) ==")
    build = run(["make", "-C", str(QEMU_DIR), "all"], ROOT, 300)
    if build.returncode != 0:
        sys.stderr.write(build.stdout)
        sys.stderr.write(build.stderr)
        print("FAIL: qemu build", file=sys.stderr)
        return 1

    TRACE.parent.mkdir(parents=True, exist_ok=True)
    qemu = run(
        ["make", "-C", str(QEMU_DIR), "run", "QEMU_LOG=1"], ROOT, 300
    )
    out = qemu.stdout + qemu.stderr
    sys.stdout.write(out)
    if qemu.returncode != 0:
        print("FAIL: qemu run (expected exit 33)", file=sys.stderr)
        return 1

    if TRACE.is_file():
        out = TRACE.read_text(encoding="utf-8", errors="replace")

    missing = [s for s in REQUIRED_BOOT + REQUIRED_HW + REQUIRED_ALGO if s not in out]
    if missing:
        print("FAIL: 缺少必需的输出行:", file=sys.stderr)
        for line in missing:
            print("  -", line, file=sys.stderr)
        return 1

    # 任何 FAIL 行都不允许出现
    for line in out.splitlines():
        if line.startswith("FAIL") or line.startswith("FATAL"):
            print(f"FAIL: 输出中出现失败行: {line}", file=sys.stderr)
            return 1

    # 缺页计数必须是 4（既不是 0 也不是重复计数）
    m = re.search(r"hw-exp05: #PF faults=(\d+) frames=(\d+)", out)
    if not m or int(m.group(1)) != 4 or int(m.group(2)) != 4:
        print("FAIL: exp05 缺页/帧计数不符合预期", file=sys.stderr)
        return 1

    # 磁盘回读字节数必须大于 0
    m = re.search(r"hw-exp07: tree persisted to LBA \d+, bytes=(\d+)", out)
    if not m or int(m.group(1)) <= 0:
        print("FAIL: exp07 目录树未写入扇区", file=sys.stderr)
        return 1

    print("PASS: verify.py (host + qemu hardware + algorithms)")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.TimeoutExpired as exc:
        print(f"FAIL: 超时 {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
