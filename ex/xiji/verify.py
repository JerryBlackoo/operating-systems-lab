#!/usr/bin/env python3
"""本机算法 + QEMU 硬件路径一键验收。"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
QEMU_DIR = ROOT / "qemu"

REQUIRED_HW = [
    "hw-lab1:",
    "PASS hw-exp01-02-pit-pic",
    "PASS hw-exp03-physical-ram",
    "PASS hw-exp04-identity-map",
    "PASS hw-exp05-cr0-pagefault",
    "PASS hw-exp06-ide-status",
    "PASS hw-exp07-ide-lba",
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


def run(cmd: list[str], cwd: Path | None = None, capture: bool = False) -> subprocess.CompletedProcess:
    return subprocess.run(
        cmd,
        cwd=cwd or ROOT,
        capture_output=capture,
        text=True,
        check=False,
    )


def main() -> int:
    print("== xiji verify: host algorithms ==")
    host = run(["make", "test"], capture=True)
    sys.stdout.write(host.stdout)
    sys.stderr.write(host.stderr)
    if host.returncode != 0:
        print("FAIL: make test", file=sys.stderr)
        return 1

    print("== xiji verify: QEMU (TCG) ==")
    build = run(["make", "-C", str(QEMU_DIR), "all"], capture=True)
    if build.returncode != 0:
        sys.stderr.write(build.stderr)
        return 1

    qemu = run(
        [
            "make",
            "-C",
            str(QEMU_DIR),
            "run",
            "QEMU_LOG=1",
        ],
        capture=True,
    )
    out = qemu.stdout + qemu.stderr
    sys.stdout.write(out)
    if qemu.returncode != 0:
        print("FAIL: make qemu run", file=sys.stderr)
        return 1

    trace = ROOT / "build" / "traces" / "qemu.stdout"
    if trace.is_file():
        out = trace.read_text(encoding="utf-8", errors="replace")

    missing = [s for s in REQUIRED_HW + REQUIRED_ALGO if s not in out]
    if missing:
        print("FAIL: missing output lines:", file=sys.stderr)
        for line in missing:
            print("  -", line, file=sys.stderr)
        return 1

    if not re.search(r"stage1: hello from MBR", out):
        print("FAIL: BIOS stage1 missing", file=sys.stderr)
        return 1

    print("PASS: verify.py (host + qemu hardware + algorithms)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())