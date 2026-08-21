#!/usr/bin/env python3
"""Extract dlopen_dso.so from sdcard-rv.img FAT32 partition."""
import struct
import os
import sys

img_path = "testdata/sdcard-rv.img"
out_dir = "tools/dso_extracted"
os.makedirs(out_dir, exist_ok=True)

f = open(img_path, "rb")

# Read MBR partition table
f.seek(446)
partitions = []
for i in range(4):
    entry = f.read(16)
    status, ptype = entry[0], entry[4]
    lba_start = struct.unpack('<I', entry[8:12])[0]
    lba_size = struct.unpack('<I', entry[12:16])[0]
    print(f"Partition {i}: status=0x{status:02x} type=0x{ptype:02x} lba_start={lba_start} sectors={lba_size}")
    if lba_size > 0:
        partitions.append((lba_start, lba_size, ptype))

f.close()
print(f"\nFound {len(partitions)} partitions")
print(f"Image size: {os.path.getsize(img_path)} bytes")
