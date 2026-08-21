# LTP RamFS Symlink Progress

Date: 2026-06-16 CST.

## Scope

This packet covers minimal symbolic-link support for RamFS, aimed at LTP
temporary directories under `/tmp`.

The honest scoring result from this follow-up is:

```text
LA ltp-glibc cap350: 94 returned 0 -> 96 returned 0
Delta: +2 returned-count cases
```

## Root Cause Addressed

LTP creates temporary directories under `/tmp`. In the current mount setup,
`/tmp` is backed by RamFS, not ext4. RamFS had hard-link-like `RamLink`
support, but did not implement `symlink()`, so the default VFS method returned
`EPERM`.

Before this change, early LTP cases broke during setup:

```text
access02.c:175: TBROK: symlink(file_f,symlink_f) failed: EPERM (1)
FAIL LTP CASE access02 : 2

access04.c:102: TBROK: symlink(symlink1,symlink2) failed: EPERM (1)
FAIL LTP CASE access04 : 2
```

RamFS now stores symbolic links as their own inode container variant:

```text
RamSymlinkInner { name, target }
FileContainer::Symlink
RamSymlink::stat() -> StatMode::LINK
RamSymlink::resolve_link() -> target
RamDir::symlink(name, target)
```

Existing `RamLink` behavior remains unchanged for hard-link-style aliases.

## Fresh Verification

All commands below ran on the remote WSL host
`lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the official
`zhouzhouyi/os-contest:20260510` image. The harness reported QEMU 10.0.2 with
its standard warning that the contest README records 9.2.1.

### Formatting Check

Command:

```text
cargo fmt --manifest-path kernel/Cargo.toml --all -- --check
```

Result:

```text
exit code 0
```

### Clean Build

Log:

```text
.codex-logs/clean-build-ramfs-symlink.log
```

Result:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

### Basic-4 Gate

These were run with freshly unpacked, basic-only RV/LA images so the harness
ended immediately after the two required basic groups.

Logs:

```text
/tmp/basic-only-rv-symlink.log
/tmp/basic-only-la-symlink.log
```

Results:

```text
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

### Targeted Short LA LTP Window

Log:

```text
/tmp/la-ltp-symlink-cap12-after.log
```

Result:

```text
RUN LTP CASE access02
FAIL LTP CASE access02 : 0

RUN LTP CASE access04
FAIL LTP CASE access04 : 0

initproc ltp: attempted 12 of 2840 cases, 5 returned 0
```

The previous `SAFE_SYMLINK ... EPERM` setup failure no longer appears.

### Full LA cap350 Comparison

Previous log:

```text
/tmp/la-cap350-after-mkfifo.log
```

Previous result:

```text
RUN LTP CASE access02
access02.c:175: TBROK: symlink(file_f,symlink_f) failed: EPERM (1)
FAIL LTP CASE access02 : 2

RUN LTP CASE access04
access04.c:102: TBROK: symlink(symlink1,symlink2) failed: EPERM (1)
FAIL LTP CASE access04 : 2

initproc ltp: attempted 350 of 2840 cases, 94 returned 0
```

Current log:

```text
/tmp/la-cap350-after-symlink.log
```

Current result:

```text
RUN LTP CASE access02
FAIL LTP CASE access02 : 0

RUN LTP CASE access04
FAIL LTP CASE access04 : 0

initproc ltp: attempted 350 of 2840 cases, 96 returned 0
```

Summary counters:

```text
previous cap350: RUN=350, TPASS=621, TFAIL=155, TBROK=70, returned0=94
current  cap350: RUN=350, TPASS=635, TFAIL=173, TBROK=68, returned0=96
```

## Non-Counted Follow-Up Evidence

`creat06` also moved past its prior symlink setup failure:

```text
previous:
creat06.c:122: TBROK: symlink(file7,test_file_eloop2) failed: EPERM (1)
FAIL LTP CASE creat06 : 2

current:
creat06.c:96: TFAIL: call succeeded unexpectedly
creat06.c:101: TPASS: got expected failure: ENAMETOOLONG (36)
creat06.c:101: TPASS: got expected failure: ENOENT (2)
creat06.c:101: TPASS: got expected failure: ENOTDIR (20)
creat06.c:131: TBROK: mmap((nil),1,PROT_NONE(0),34,0,0) failed: EAGAIN/EWOULDBLOCK (11)
FAIL LTP CASE creat06 : 2
```

This is useful root-cause progress, but it is not counted as score growth.

## Remaining Known Gaps

- Symlink resolution still follows the existing `File::open_link()` behavior,
  including its current relative-target limitations.
- `access04` is a returned-0 case, but still contains internal `TFAIL` lines
  around EINVAL/ENOENT/ELOOP/EROFS expectations.
- `creat06` still returns 2 after reaching later path/error-address checks.
- The cap350 window still contains unrelated LTP blockers such as cgroups,
  bad-address mmap probes, missing device acquisition, and `epoll_pwait03`
  watchdog behavior.
