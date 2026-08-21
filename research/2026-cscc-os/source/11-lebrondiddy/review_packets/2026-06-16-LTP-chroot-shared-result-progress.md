# LTP chroot Shared Result Progress

Date: 2026-06-16 CST.

## Scope

This packet covers the LA `ltp-glibc` `chroot01`, `chroot02`, and `chroot03`
blockers, plus the shared-memory result propagation issue exposed after
`chroot(2)` started working.

Honest targeted delta:

```text
chroot02: 0 returned 0 -> 1 returned 0
chroot03: targeted case 1 -> 0 after chroot landed
chroot01: targeted case 1 -> 0 after credentials were tracked
```

Raw returned-count caveat:

```text
chroot02 is a real returned-count gain in the current cap350 window.
chroot03 is a clean-quality/TPASS improvement; older cap350 windows already
reported that case as process status 0 despite internal TFAIL lines.
chroot01 recovers the post-chroot permission regression in the current window;
older cap350 windows may already have counted it as process status 0 while
still printing internal ENOSYS/TFAIL lines.
```

No OJ submission was made.

## Root Causes Addressed

`chroot02` had two independent blockers.

First, the kernel did not dispatch `chroot(2)`, so the test failed immediately:

```text
.codex-logs/la-target-chroot02-red-20260616.log
RUN LTP CASE 000_chroot02
chroot02.c:30: TFAIL: chroot(/tmp/LTP_000w2Hych) failed: ENOSYS (38)
tst_test.c:1449: TBROK: Test haven't reported results!
FAIL LTP CASE 000_chroot02 : 2
initproc ltp: attempted 1 of 2841 cases, 0 returned 0
```

The narrow syscall fix adds `Sysno::chroot`, stores a per-process `root_dir`,
copies it across `fork()`, and resolves absolute paths under that root when a
process has been chrooted.

After that first fix, `chroot()` and the in-root `stat()` both passed, but the
whole LTP case still returned broken:

```text
.codex-logs/la-target-chroot02-green-20260616.log
RUN LTP CASE 000_chroot02
chroot02.c:30: TPASS: chroot(/tmp/LTP_0006KI8lD) passed
chroot02.c:34: TPASS: stat(/chroot02_testfile) passed
tst_test.c:1449: TBROK: Test haven't reported results!
FAIL LTP CASE 000_chroot02 : 2
initproc ltp: attempted 1 of 2841 cases, 0 returned 0
```

That second failure was not chroot semantics. It was LTP result IPC. The harness
uses shared anonymous mappings for result counters, but `cow_fork()` remapped
every VMA as `URX` in both parent and child. For `MemType::Shared`, a child
write then triggered `user_cow_int()`, whose shared-memory path deliberately
signals `SIGSEGV` instead of copying the page. The child printed TPASS lines, but
the parent never received valid shared counters, so LTP reported:

```text
TBROK: Test haven't reported results!
```

The fork fix is intentionally limited:

```text
MemType::Shared | MemType::ShareFile => MappingFlags::URWX
private mappings                       => MappingFlags::URX
```

Private mappings still use the existing COW path. Shared mappings now stay
writable and shared across `fork()`.

Second, `chroot03` exposed a final symlink semantics gap. Before this follow-up,
four error checks passed, but a symlink loop returned `ENOTDIR` because
`chroot()` opened the final path without following symlinks:

```text
.codex-logs/la-target-chroot03-red-20260616.log
RUN LTP CASE 000_chroot03
chroot03.c:54: TPASS: chroot(longer than VFS_MAXNAMELEN) : ENAMETOOLONG (36)
chroot03.c:54: TPASS: chroot(not a directory) : ENOTDIR (20)
chroot03.c:54: TPASS: chroot(does not exists) : ENOENT (2)
chroot03.c:54: TPASS: chroot(an invalid address) : EFAULT (14)
chroot03.c:54: TFAIL: chroot(symlink loop) expected ELOOP: ENOTDIR (20)
FAIL LTP CASE 000_chroot03 : 1
initproc ltp: attempted 1 of 2841 cases, 0 returned 0
```

The follow-up is also narrow:

- `File::open_link()` now resolves relative symlink targets from the symlink's
  containing directory instead of from `/`.
- Exceeding the symlink-follow limit returns `ELOOP` instead of `EINVAL`.
- `sys_chroot()` uses `File::open_link()` for the target path, then still
  requires the resolved target to be a directory.

Third, once `chroot(2)` existed, `chroot01` exposed that identity syscalls were
only stubs. LTP drops to the `nobody` user with `seteuid()` and expects an
unprivileged `chroot()` attempt to fail with `EPERM`, but the kernel still
reported every process as uid/euid 0:

```text
.codex-logs/la-cap350-chroot-20260616.log
RUN LTP CASE chroot01
chroot01.c:23: TFAIL: unprivileged chroot() succeeded
FAIL LTP CASE chroot01 : 1
```

The narrow credentials fix adds real/effective/saved uid/gid fields to the
process control block, copies them across `fork()`, makes `get*id()` and
`set*id()` observe/update those fields, and rejects `chroot()` with `EPERM` when
the effective uid is nonzero.

## Fresh Verification

All commands below ran on the remote WSL host
`lenovo@100.92.143.96:/home/lenovo/oskernel2025-x` in the official
`zhouzhouyi/os-contest:20260510` image.

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
.codex-logs/clean-build-credentials-chroot01-20260616.log
```

Result:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

### Basic-4 Gate

Logs:

```text
.codex-logs/basic-only-rv-credentials-chroot01-20260616.log
.codex-logs/basic-only-la-credentials-chroot01-20260616.log
```

Results:

```text
RISC-V: basic-glibc and basic-musl START/END markers found; both include brk
        through yield output.
LoongArch: basic-glibc and basic-musl START/END markers found; both include brk
           through yield output.
```

Note: these logs used the full OJ images, so the harness continued into later
non-basic groups after the basic markers. The later groups are not part of this
basic-4 claim.

### Targeted chroot01 LTP Walker

The target image was a disposable single-case LTP image generated from the
writeable LA glibc LTP base image:

```text
/tmp/la-ltp-chroot01-final-20260616.img
```

It links `chroot01` as `000_chroot01` and sets the LTP profile case limit to 1.
The normal initproc LTP walker and harness were unchanged.

Log:

```text
.codex-logs/la-target-chroot01-final-20260616.log
```

Result:

```text
RUN LTP CASE 000_chroot01
chroot01.c:23: TPASS: unprivileged chroot() : EPERM (1)

Summary:
passed   1
failed   0
broken   0
warnings 0
FAIL LTP CASE 000_chroot01 : 0
initproc ltp: attempted 1 of 2841 cases, 1 returned 0
```

### Targeted LTP Walker

The target image was a disposable single-case LTP image:

```text
/tmp/la-ltp-chroot02-red-20260616.img
```

It links `chroot02` as `000_chroot02` and sets the LTP profile case limit to 1.
The normal initproc LTP walker and harness were unchanged.

Original green log after the shared-result fix:

```text
.codex-logs/la-target-chroot02-final-20260616.log
```

Result:

```text
RUN LTP CASE 000_chroot02
chroot02.c:30: TPASS: chroot(...) passed
chroot02.c:34: TPASS: stat(/chroot02_testfile) passed

Summary:
passed   2
failed   0
broken   0
warnings 0
FAIL LTP CASE 000_chroot02 : 0
initproc ltp: attempted 1 of 2841 cases, 1 returned 0
```

### Targeted chroot03 LTP Walker

The target image was a disposable single-case LTP image:

```text
/tmp/la-ltp-chroot03-red-20260616.img
```

It links `chroot03` as `000_chroot03` and sets the LTP profile case limit to 1.
The normal initproc LTP walker and harness were unchanged.

Log:

```text
.codex-logs/la-target-chroot03-final-20260616.log
```

Result:

```text
RUN LTP CASE 000_chroot03
chroot03.c:54: TPASS: chroot(longer than VFS_MAXNAMELEN) : ENAMETOOLONG (36)
chroot03.c:54: TPASS: chroot(not a directory) : ENOTDIR (20)
chroot03.c:54: TPASS: chroot(does not exists) : ENOENT (2)
chroot03.c:54: TPASS: chroot(an invalid address) : EFAULT (14)
chroot03.c:54: TPASS: chroot(symlink loop) : ELOOP (40)

Summary:
passed   5
failed   0
broken   0
warnings 0
FAIL LTP CASE 000_chroot03 : 0
initproc ltp: attempted 1 of 2841 cases, 1 returned 0
```

The remaining `TWARN` is cleanup noise from removing a path after the process is
inside the chroot. It does not affect the LTP result:

```text
tst_tmpdir.c:342: TWARN: tst_rmdir: rmobj(...) failed: ... errno=20: ENOTDIR
```

### cap350 Exploratory Window

An exploratory LA `ltp-glibc` cap350 run was started after the chroot/shared
changes:

```text
.codex-logs/la-cap350-chroot-20260616.log
```

It did not complete the full 350-case window. The run reached 225 LTP cases and
then QEMU was terminated by the outer 480s timeout:

```text
RUN=225
TPASS=350
TFAIL=252
TBROK=31
qemu-system-loongarch64: terminating on signal 15 from pid 29 (timeout)
```

The covered chroot window showed the pre-credentials `chroot01/chroot04`
permission failures alongside the already-fixed `chroot02/chroot03` cases:

```text
chroot01: TFAIL unprivileged chroot() succeeded, FAIL LTP CASE chroot01 : 1
chroot02: 2 TPASS, FAIL LTP CASE chroot02 : 0
chroot03: 5 TPASS, FAIL LTP CASE chroot03 : 0
chroot04: TFAIL expected EACCES, got EPERM, FAIL LTP CASE chroot04 : 1
```

Because the run timed out before 350 cases, this packet does not claim a broad
cap350 returned-count delta from that exploratory log.

## Remaining Known Gaps

- This is not a full Linux `chroot(2)` security model. It is the minimal path
  resolution behavior needed by the current process model and the observed LTP
  cases.
- `chroot04` still requires real file mode/search-permission semantics. After
  credentials landed, it now reaches the expected class of failure but still
  returns `EPERM` where LTP expects `EACCES`.
- The cap350 exploratory run timed out before completing the 350-case window, so
  no broad cap350 delta is claimed.
- The shared-mapping fix likely helps other LTP cases that fork and report
  through `MAP_SHARED|MAP_ANONYMOUS`, but this packet only claims the proven
  targeted `chroot01`, `chroot02`, and `chroot03` improvements.
