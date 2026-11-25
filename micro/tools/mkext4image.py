#!/usr/bin/env python3
"""Build a small ext4 root image with /motd (and optional largefile) for M5 QEMU."""
from __future__ import annotations

import hashlib
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

DEFAULT_MOTD = "microkernel motd v1\n"
IMAGE_MB = 32


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    argc = len(sys.argv)
    out = Path(sys.argv[1]) if argc > 1 else Path("build/micro-root.img")
    motd = Path(sys.argv[2]).read_text(encoding="utf-8") if argc > 2 else DEFAULT_MOTD

    mke2fs = shutil.which("mke2fs")
    if not mke2fs:
        print("[mkext4image] mke2fs not found (install e2fsprogs)", file=sys.stderr)
        return 2

    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="micro-ext4-") as tmp:
        root = Path(tmp) / "root"
        root.mkdir()
        (root / "motd").write_text(motd, encoding="utf-8")
        large = root / "largefile"
        large.write_bytes(b"L" * (200 * 1024))

        cmd = [
            mke2fs,
            "-t",
            "ext4",
            "-b",
            "4096",
            "-O",
            "extent,^64bit",
            "-F",
            "-d",
            str(root),
            str(out),
            f"{IMAGE_MB}M",
        ]
        print("[mkext4image]", " ".join(cmd))
        subprocess.check_call(cmd)

    (out.parent / "micro-motd.txt").write_text(motd, encoding="utf-8")
    digest = sha256_file(out)
    print(f"[mkext4image] wrote {out} ({out.stat().st_size} bytes)")
    print(f"[mkext4image] image sha256={digest}")
    print(f"[mkext4image] motd sha256={hashlib.sha256(motd.encode()).hexdigest()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())