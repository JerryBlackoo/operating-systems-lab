# LTP Identity Files Progress

Date: 2026-06-16 CST.

## Scope

This packet covers a small LTP test-environment seed fix for `/etc/passwd` and
`/etc/group`.

Honest scoring result:

```text
LA ltp-glibc cap350: 109 returned 0 -> 110 returned 0
Delta: +1 returned-count case
```

## Root Cause Addressed

`chmod07` did not reach its actual chmod-mode assertions because LTP could not
resolve either the primary group name `users` or its fallback group `daemon`:

```text
chmod07.c:77: TINFO: getgrnam(users) failed - try fallback daemon
chmod07.c:77: TBROK: getgrnam(daemon) failed: SUCCESS (0)
FAIL LTP CASE chmod07 : 2
```

The kernel already seeds minimal identity files before running test groups, but
the seeded `/etc/group` only contained `root`, `nogroup`, and `nobody`.

## Fix

`seed_etc_identity_files()` now includes the common static identities needed by
this LTP path:

- `/etc/passwd`: add `daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin`
- `/etc/group`: add `daemon:x:1:` and `users:x:100:`

This is deliberately an environment seed fix. It does not claim to implement
full Unix account management or file permission semantics.

## Verification

All commands below ran on the remote WSL host
`lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the official
`zhouzhouyi/os-contest:20260510` image.

### Format and Clean Build

Command:

```text
cargo fmt --manifest-path kernel/Cargo.toml --all -- --check
rm -rf kernel/target kernel-rv kernel-la kernel/mount.img
make -B all
file kernel-rv kernel-la
readelf -h kernel-rv | grep -i flags
```

Result:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

### Basic-4 Gate

Logs:

```text
.codex-logs/basic-only-rv-identity-files-20260616.log
.codex-logs/basic-only-la-identity-files-20260616.log
```

Results:

```text
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

### LA cap350 Comparison

Previous log:

```text
.codex-logs/la-cap350-clock-efault-20260616.log
```

Previous result:

```text
RUN=350 returned0=109 TPASS=657 TFAIL=228 TBROK=49
```

Current log:

```text
.codex-logs/la-cap350-identity-files-20260616.log
```

Current result:

```text
RUN=350 returned0=110 TPASS=657 TFAIL=229 TBROK=48
initproc ltp: attempted 350 of 2840 cases, 110 returned 0
```

Target case after the fix:

```text
RUN LTP CASE chmod07
chmod07.c:61: TFAIL: testfile: Incorrect modes 0100700; expected 01777
FAIL LTP CASE chmod07 : 0
```

`chmod07` still has a real internal chmod/stat mode failure. The honest gain is
only that the case no longer exits through a `getgrnam()` environment `TBROK`
and now returns 0 as a whole LTP case.

## Notes

- No OJ submission was made.
- No garbage `TBROK` growth was counted.
- The remaining chmod/chown failures point at file metadata mode/ownership
  semantics and should be handled separately.
