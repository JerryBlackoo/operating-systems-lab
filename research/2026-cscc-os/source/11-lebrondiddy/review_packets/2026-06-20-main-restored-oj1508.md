# 2026-06-20 Main Restored To OJ1508 Baseline

Date: 2026-06-20

## Scope

This packet records the `main` protection action after `main/ea02955` scored
only `1351.584` on OJ and exposed the `nanosleep04` time overflow panic during
post-`unlinkat` full calibration.

The protected OJ baseline remains `e206a190994f3456d9b024aed1e3675b2d3696e2`
(`oj-busybox-bank-20260619`), which scored `1508.5793028047033`.

## Branch Actions

Local and remote state before protection:

```text
local main:  ea02955a44e8f1db5bcc78dfaa426a0c8a82f38d
origin/main: ea02955a44e8f1db5bcc78dfaa426a0c8a82f38d
```

The investigation branch was created before changing `main`:

```text
wip/time-nanosleep-overflow-20260620 -> ea02955a44e8f1db5bcc78dfaa426a0c8a82f38d
```

`main` was restored non-destructively with:

```text
git revert -m 1 1577efa --no-edit
```

Resulting restore commit:

```text
71bcf85271f03a087b6bc2c1414a1d93311c323b Revert "Merge LA LTP direct unlinkat fix"
```

This removes the post-`e206a19` kernel `fd.rs` behavior change from `main`.
The remaining diff versus `e206a19` is documentation only:

```text
PLAN.md
docs/oj-scores.csv
review_packets/2026-06-19-OJ1508-busybox-bank-result.md
```

## Build Evidence

Remote clean detached worktree:

```text
/home/yxzhang/oskernel-worktrees/main-restored-oj1508-20260620-121105
HEAD 71bcf85271f03a087b6bc2c1414a1d93311c323b
```

Clean build command ran inside `zhouzhouyi/os-contest:20260510` after removing
`kernel/target`, `kernel-rv`, `kernel-la`, and `kernel/mount.img`:

```text
/home/yxzhang/oskernel-artifacts/main-restored-oj1508-20260620/clean-build-all-main-restored-20260620-121105.log
```

Build outputs:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
readelf kernel-rv Flags: 0x1, RVC, soft-float ABI

7cf10d2e303e778228f054991bd03259393f6ef721634784231b36c32ed7604b  kernel-rv
35b1475241cc2901bbe876344a034275bda90ad1341ea8e4cd0ca592789e8509  kernel-la
```

## Basic Quick Gate

The quick gate used generated 96M nested basic images from the baseline payload
under `/home/yxzhang/oskernel2025-x/.m0-baselines/ByteOS/tools`. This is a
basic-only quick gate, not score evidence.

Valid driver log:

```text
/home/yxzhang/oskernel-artifacts/main-restored-oj1508-20260620/basic-quick-main-restored-20260620-121105-marker-basic.log
```

Results:

```text
BASIC_RV_RC 0
PASS: RISC-V basic markers and real basic case output found

BASIC_LA_RC 0
PASS: LoongArch basic markers and real basic case output found
```

An earlier quick-gate attempt used `EXPECTED_GROUPS=basic-glibc,basic-musl`,
but the repo's generated quick image preserves the upstream marker text
`basic` for both libc directories. That attempt failed only at marker-name
validation after both RV basic bodies completed; it is not counted as the gate.

## BusyBox-Musl Targeted Gate

The targeted BusyBox-musl gate used fresh disposable copies of the staged 4G
official images:

```text
/home/yxzhang/oscomp-sdcard/sdcard-rv.img
/home/yxzhang/oscomp-sdcard/sdcard-la.img
```

Only non-target `*_testcode.sh` scripts were pruned. The target files
`/musl/busybox_testcode.sh`, `/musl/busybox_cmd.txt`, and `/musl/busybox`
were verified with `debugfs`.

Driver log:

```text
/home/yxzhang/oskernel-artifacts/main-restored-oj1508-20260620/busybox-musl-target-main-restored-20260620-121105-driver.log
```

RISC-V:

```text
source sha256 e7fdcf94d39ed39c547d076ea37369b5356414bdcf1e0a1c72c5ba294288ce71
run image sha256 0441657570e97c54837043b0959f819c17a326ee23c64b82bac309f111c6bda8
BUSYBOX_rv_RC 0
SUMMARY total=53 success=53 fail=0
PASS: RISC-V busybox-musl markers and real basic case output found
```

LoongArch:

```text
source sha256 d86a488d990715635b1d68802c39f49f05328fef3536c91c4d02e41cb00a12d8
run image sha256 aa9fb45e7712aab1b5b50f2360f553b3878e4210505d31c628c1c9b92adc23a9
BUSYBOX_la_RC 0
SUMMARY total=53 success=52 fail=1
testcase busybox sh -c 'sleep 5' & ./busybox kill $! fail
PASS: LoongArch busybox-musl markers and real basic case output found
```

The historical packet used a 55-command denominator. This packet records the
actual emitted `testcase busybox` result-line count from this run (`53`); the
only LA failure is still the known `sleep 5` / `kill` line.

## Decision

`main` at `71bcf85` is the protected OJ1508-equivalent code baseline. It is safe
to tag as:

```text
oj1508-restored-20260620
```

`ea02955` is not an OJ candidate despite being accepted, because its OJ total
(`1351.584`) is below the banked `e206a19` result (`1508.5793028047033`) and it
exposes the `nanosleep04` time overflow panic before entering `ltp-musl` in the
LA full/OJ-like calibration.
