#!/usr/bin/env python3
"""Pack build/user/*.elf into micro initrd (MIRD v1). See tools/README-initrd.md."""
import struct
import sys
from pathlib import Path

MAGIC = b"MIRD"
VERSION = 1


def pack_member(name: bytes, data: bytes) -> bytes:
    if len(name) > 255:
        raise ValueError(f"name too long: {name!r}")
    hdr = struct.pack("<II", len(name), len(data))
    body = hdr + name + data
    pad = (4 - (len(body) % 4)) % 4
    return body + (b"\x00" * pad)


def main() -> int:
    if len(sys.argv) < 3:
        print("usage: mkinitrd.py <output.img> <member>...", file=sys.stderr)
        return 2
    out = Path(sys.argv[1])
    members = []
    for path in sys.argv[2:]:
        p = Path(path)
        data = p.read_bytes()
        name = p.stem.encode("ascii")
        members.append((name, data))
    body = b"".join(pack_member(n, d) for n, d in members)
    blob = MAGIC + struct.pack("<II", VERSION, len(members)) + body
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(blob)
    print(f"[mkinitrd] wrote {out} ({len(blob)} bytes, {len(members)} members)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())