# 2026-06-17 LTP execve03 remote verification

## Scope

Branch:

```text
wip/ltp-reach-20260616
```

Goal: verify the `execve03` panic fix on the new remote server, preserve the
musl LTP gains, and commit the accepted fix. This packet uses the official
`zhouzhouyi/os-contest:20260510` image on:

```text
ssh yxzhang@10.102.35.44
```

The official Docker image was initially absent on the server and was loaded
there before this run. Verification command:

```sh
ssh yxzhang@10.102.35.44 'docker images | grep os-contest'
```

Result:

```text
zhouzhouyi/os-contest   20260510   60e9bfa0ecdc   5 weeks ago   19.6GB
```

## Fix Under Test

`kernel/kernel/src/tasks/exec.rs` no longer panics when `execve()` is asked to
load an empty/non-ELF file:

- `file_size == 0` returns `ENOEXEC` before frame allocation;
- failed ELF buffer allocation returns `ENOMEM`;
- the loader avoids direct `[0]` indexing on the frame vector;
- short reads, invalid ELF magic, malformed program headers, and empty load
  segment sets return `ENOEXEC`;
- loadable segments are validated before replacing the old address space;
- segment mapping failure returns `ENOMEM`.

This directly covers the prior M3 panic:

```text
RUN LTP CASE execve03
panic: 'index out of bounds: the len is 0 but the index is 0'
```

## Build Gate

Remote command:

```sh
docker run --rm --network none -e RUSTUP_TOOLCHAIN=nightly-2025-01-18 \
  -v /home/yxzhang/oskernel2025-x:/workspace -w /workspace \
  zhouzhouyi/os-contest:20260510 \
  bash -lc 'rm -rf kernel/target kernel-rv kernel-la kernel/mount.img; \
            make -B all; file kernel-rv kernel-la; \
            readelf -h kernel-rv | grep -i flags'
```

Result:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, ...
kernel-la: ELF 64-bit LSB executable, LoongArch, ...
Flags: 0x1, RVC, soft-float ABI
```

## Basic-4 Gate

The small repository sdcard images contain only `basic_testcode.sh` and print
the generic group name `basic`. For libc-specific evidence, this run reused
fresh disposable marked basic-only images in `~/oskernel-artifacts`.

Logs:

```text
/home/yxzhang/oskernel-artifacts/basic-marked-rv-execve03-20260617-freshbuild.log
/home/yxzhang/oskernel-artifacts/basic-marked-la-execve03-20260617-freshbuild.log
```

Evidence:

```text
RISC-V:
START basic-glibc
END   basic-glibc
START basic-musl
END   basic-musl
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

LoongArch:
START basic-glibc
END   basic-glibc
START basic-musl
END   basic-musl
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## LTP Method

Runner:

```text
/home/yxzhang/oskernel-artifacts/run-ltp-quadrant-execve03.sh
```

Inputs:

```text
/home/yxzhang/oscomp-sdcard/sdcard-rv.img
/home/yxzhang/oscomp-sdcard/sdcard-la.img
```

Each quadrant used a fresh image copy. The prep step removed all non-target
`*_testcode.sh` files, including `unixbench_testcode.sh`, removed
`.ltp_case_limit`, wrote `/.watchdog_global_deadline_secs = 6600`, and kept
exactly the target libc `ltp_testcode.sh`.

Prep evidence:

```text
rv-glibc remaining target scripts: /glibc/ltp_testcode.sh
rv-musl  remaining target scripts: /musl/ltp_testcode.sh
la-glibc remaining target scripts: /glibc/ltp_testcode.sh
la-musl  remaining target scripts: /musl/ltp_testcode.sh
remaining other scripts: empty in all four quadrants
```

Run settings:

```text
TIMEOUT_SECONDS=6600
STALL_TIMEOUT_SECONDS=600
EXPECTED_GROUPS=ltp-glibc or ltp-musl
```

Counting rule: only real LTP case exit code 0 is counted, via
`FAIL LTP CASE ... : 0` and the guest `returned 0` summary. TBROK and harness
garbage are not counted as passes.

## LTP Result

Baseline is `review_packets/2026-06-16-LTP-baseline-6600.md`.

| Quadrant | Baseline returned0 | Current RUN / returned0 | Delta | execve03 | Terminal state |
| --- | ---: | ---: | ---: | --- | --- |
| rv-glibc | 173 | 2840 / 210 | +37 | `FAIL ... execve03 : 1` | full walk, END `ltp-glibc` |
| rv-musl | 162 | 2820 / 215 | +53 | `FAIL ... execve03 : 2` | full walk, END `ltp-musl` |
| la-glibc | 153 | 978 / 192 | +39 | `FAIL ... execve03 : 1` | post-execve03 page fault at `ima_kexec.sh` |
| la-musl | 173 | 2820 / 224 | +51 | `FAIL ... execve03 : 2` | full walk, END `ltp-musl` |

Aggregate:

```text
baseline returned0: 661
current  returned0: 841
delta: +180
```

The two glibc quadrants no longer panic at `execve03`; both continue past that
case and exceed their baseline returned0 thresholds. The remaining `la-glibc`
terminal state is later than `execve03`:

```text
RUN LTP CASE ima_conditionals.sh
FAIL LTP CASE ima_conditionals.sh : 139
RUN LTP CASE ima_kexec.sh
panic: 'page fault: LoadPageFault(0x1201bdc58)'
```

This is a separate post-`execve03` reach wall, not the fixed empty-file loader
panic.

## Remote Logs

```text
/home/yxzhang/oskernel-artifacts/ltp-rv-glibc-execve03-6600-20260617.log
/home/yxzhang/oskernel-artifacts/ltp-rv-musl-execve03-6600-20260617.log
/home/yxzhang/oskernel-artifacts/ltp-la-glibc-execve03-6600-20260617.log
/home/yxzhang/oskernel-artifacts/ltp-la-musl-execve03-6600-20260617.log

/home/yxzhang/oskernel-artifacts/ltp-*-execve03-6600-20260617.summary
/home/yxzhang/oskernel-artifacts/ltp-*-execve03-prep-20260617.log
/home/yxzhang/oskernel-artifacts/ltp-*-execve03-driver-20260617.log
```

## Decision

Acceptance gates for this task pass:

- clean official-image build passed;
- `kernel-rv` is soft-float;
- basic-4 passed;
- no `execve03` kernel panic in either glibc quadrant;
- `rv-glibc >= 173` and `la-glibc >= 153`;
- musl gains are preserved near the M3 values;
- aggregate returned0 is greater than the 661 baseline.

Stop after committing this branch packet. Do not merge to `main`, push, or touch
OJ from this task.
