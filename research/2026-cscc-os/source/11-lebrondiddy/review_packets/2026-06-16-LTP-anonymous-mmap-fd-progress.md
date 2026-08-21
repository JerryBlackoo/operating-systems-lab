# LTP Anonymous Mmap FD Progress

Date: 2026-06-16 CST.

## Scope

This packet covers a minimal `mmap()` semantics fix for anonymous mappings.
The target symptom was LA LTP cases that passed `MAP_PRIVATE|MAP_ANONYMOUS`
with `fd=0` and then failed while the kernel tried to read stdin.

The honest scoring result from this follow-up is:

```text
LA ltp-glibc cap350: 96 returned 0 -> 105 returned 0
Delta: +9 returned-count cases
```

## Root Cause Addressed

`sys_mmap()` resolved the `fd` argument before checking whether the mapping was
anonymous:

```text
let file = self.task.get_fd(fd);
```

For `MAP_ANONYMOUS`, Linux ignores `fd`. LTP commonly passes `fd=0` in this
mode. The old behavior therefore treated stdin (`/dev/ttyv0`) as the backing
file and later attempted to read from it while faulting the mapping in. An empty
TTY returned `EWOULDBLOCK`, which surfaced in LTP as `EAGAIN/EWOULDBLOCK`.

Before this change:

```text
RUN LTP CASE chmod06
chmod06.c:112: TBROK: mmap((nil),1,PROT_NONE(0),34,0,0) failed: EAGAIN/EWOULDBLOCK (11)

RUN LTP CASE creat06
creat06.c:131: TBROK: mmap((nil),1,PROT_NONE(0),34,0,0) failed: EAGAIN/EWOULDBLOCK (11)
```

The fix is intentionally narrow:

```text
let file = if flags.contains(MapFlags::ANONYMOUS) {
    None
} else {
    Some(self.task.get_fd(fd).ok_or(Errno::EBADF)?)
};
```

Anonymous mappings now ignore `fd`; non-anonymous mappings with an invalid fd
return `EBADF`.

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
.codex-logs/clean-build-anon-mmap-fd.log
```

Result:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

### Basic-4 Gate

These were run with basic-only nested RV/LA images, so the harness ended
immediately after the two required basic groups.

Logs:

```text
.codex-logs/basic-only-rv-anon-mmap-fd.log
.codex-logs/basic-only-la-anon-mmap-fd.log
```

Results:

```text
RISC-V:
basic-glibc finished in 2s (exited, code 0)
basic-musl finished in 1s (exited, code 0)
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

LoongArch:
basic-glibc finished in 2s (exited, code 0)
basic-musl finished in 2s (exited, code 0)
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

### Full LA cap350 Comparison

Previous log:

```text
/tmp/la-cap350-after-symlink.log
```

Previous result:

```text
RUN=350
TPASS=635
TFAIL=173
TBROK=68
initproc ltp: attempted 350 of 2840 cases, 96 returned 0
```

Current log:

```text
/tmp/la-cap350-after-anon-mmap-fd.log
```

Current result:

```text
RUN=350
TPASS=639
TFAIL=231
TBROK=54
initproc ltp: attempted 350 of 2840 cases, 105 returned 0
```

### Targeted Case Evidence

`chmod06` no longer breaks at the anonymous `mmap(PROT_NONE, fd=0)` setup:

```text
RUN LTP CASE chmod06
chmod06.c:81: TFAIL: chmod succeeded unexpectedly
...
```

`creat06` also moves past the previous `mmap()` `EWOULDBLOCK` failure:

```text
RUN LTP CASE creat06
creat06.c:96: TFAIL: call succeeded unexpectedly
creat06.c:101: TPASS: got expected failure: ENAMETOOLONG (36)
creat06.c:101: TPASS: got expected failure: ENOENT (2)
creat06.c:101: TPASS: got expected failure: ENOTDIR (20)
```

## Remaining Known Gaps

- This does not fully model `PROT_NONE` permissions. Anonymous VMA permission
  preservation and missing-page fault permissions remain broader MM work.
- Several moved-forward cases still contain internal `TFAIL` lines; the counted
  improvement here is only whole-case returned-0 growth.
- The cap350 window still contains unrelated LTP blockers such as cgroups,
  device acquisition, path-permission expectations, and remaining VFS/MM gaps.
