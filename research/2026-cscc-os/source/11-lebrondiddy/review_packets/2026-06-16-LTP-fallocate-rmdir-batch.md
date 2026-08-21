# 2026-06-16 LTP batch: fallocate semantics + rmdir cleanup

## Scope

This is an M2 cheap-batch result from the full `la-musl` LTP baseline.

Baseline source:

- `.codex-logs/ltp-baseline-la-musl-6600-20260616.log`
- `.codex-logs/ltp-baseline-la-musl-6600-20260616.summary`

Baseline summary:

```text
RUN=2820 returned0=173 timeouts=96
summary=initproc ltp: attempted 2820 of 2820 cases, 173 returned 0
```

Failure buckets from the full baseline:

```text
127: 1611
32:  409
2:   177
1:   172
124: 96
139: 79
5:   44
4:   15
3:   11
33:  7
36:  7
6:   7
34:  5
255: 2
128: 1
22:  1
132: 1
35:  1
7:   1
```

## Failure table slice

| Case | Baseline failure | Root-cause cluster | Cost |
| --- | --- | --- | --- |
| `fallocate01` | `FAIL LTP CASE fallocate01 : 5`; size check failed after `fallocate(..., mode=0, ...)`, plus `tst_rmdir ... errno=ENOENT` cleanup warning | `fallocate` was fake-success and did not extend file; cleanup `remove()` on directory returned misleading `ENOENT` | ★ |
| `fallocate02` | `FAIL LTP CASE fallocate02 : 5`; invalid fd/offset/len/overflow cases returned success, plus cleanup warning | `fallocate` error matrix missing; same cleanup errno problem | ★ |
| other exit-5 cases (`fchownat*`, `fcntl*`, `fdatasync01`, `fstatat01`, `ftest*`, `lchown*`) | exit 5 in baseline, usually with `tst_rmdir ... ENOENT`; targeted bucket rerun shows real remaining TFAIL after cleanup fix | mixed syscall gaps: ownership, record locks/leases, fdatasync, fstatat, truncate/rmdir semantics | ●/✗ depending cluster |

## Implementation

Changed files:

- `kernel/kernel/src/syscall/fd.rs`
- `kernel/kernel/src/syscall/mod.rs`
- `kernel/filesystem/fs/src/file.rs`

Changes:

- Implemented `sys_fallocate(fd, mode, offset, len)` instead of unconditional `Ok(0)`.
  - supports mode `0` and `FALLOC_FL_KEEP_SIZE`
  - rejects unsupported flags and `len == 0` with `EINVAL`
  - rejects negative offset/len after syscall-width cast with `EINVAL`
  - rejects overflow / beyond signed file range with `EFBIG`
  - rejects invalid or non-writable fd with `EBADF`
  - grows the file for mode `0`; keeps size for `KEEP_SIZE`
- Added `File::rmdir_self()`.
- Made `unlinkat(..., AT_REMOVEDIR)` call directory removal instead of treating the flag as `OpenFlags`.
- Made plain `unlinkat` return `EISDIR` on directories so musl `remove()` can fall back to `rmdir`.
- Split x86 `unlink` and `rmdir` dispatch.

## Verification

Clean build and ABI:

Log: `.codex-logs/clean-build-fallocate-rmdir2-20260616.log`

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI
kernel-la: ELF 64-bit LSB executable, LoongArch
Flags: 0x1, RVC, soft-float ABI
cargo fmt --check: passed
```

Targeted red/green:

- RED: `.codex-logs/la-musl-fallocate01-02-red-20260616.log`
  - `000_fallocate01 : 5`
  - `001_fallocate02 : 5`
  - `attempted 2 ... 0 returned 0`
- After `fallocate` only: `.codex-logs/la-musl-fallocate01-02-green-20260616.log`
  - internal `fallocate01/02` assertions passed
  - cases still returned `: 4` because cleanup `tst_rmdir` warned
- GREEN after cleanup fix: `.codex-logs/la-musl-fallocate01-02-rmdir-green3-20260616.log`
  - `000_fallocate01 : 0`
  - `001_fallocate02 : 0`
  - `attempted 2 of 2 cases, 2 returned 0`

Related-bucket measurement:

Log: `.codex-logs/la-musl-exit5-bucket-current-20260616.log`

The baseline `exit=5` bucket had 44 cases. Rerunning that same bucket after this
batch produced:

```text
FAIL LTP CASE fallocate01 : 0
FAIL LTP CASE fallocate02 : 0
initproc ltp: attempted 44 of 44 cases, 2 returned 0
```

Exit-code distribution in that bucket after the fix:

```text
0: 2
1: 42
```

This means the honest measured gain for this batch is `returned0 +2` in the
related bucket. The cleanup errno fix removes a broad `TWARN` source, but it does
not unlock the remaining 42 exit-5 cases because they still have real TFAILs.

Basic-4 gate:

- `.codex-logs/basic4-rv-basic-only-fallocate-rmdir-20260616.log`
  - `PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found`
- `.codex-logs/basic4-la-basic-only-fallocate-rmdir-20260616.log`
  - `PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found`

Note: `.codex-logs/basic4-rv-fallocate-rmdir-20260616.log` used the full OJ
image and timed out later in `iozone-glibc`; it is not counted as the basic-4
gate. The basic-only logs above are the clean basic-4 evidence.

## Batch accounting

- Root-cause clusters fixed:
  - `fallocate` syscall semantics and error matrix
  - `unlinkat/rmdir` cleanup errno/dispatch path
- Code modules touched:
  - syscall fd layer
  - syscall dispatch
  - VFS `File` helper
- Clean builds:
  - 2 after this patch set (`clean-build-fallocate-rmdir`, then `clean-build-fallocate-rmdir2`)
- Targeted LTP runs:
  - 1 RED (`fallocate01/02`)
  - 1 partial GREEN (`fallocate` fixed but cleanup still failed)
  - 1 final GREEN (`fallocate01/02`)
  - 1 related exit-5 bucket (`44 cases`)
- Basic gates:
  - RV basic-only
  - LA basic-only
- Returned0 net gain measured:
  - `+2` in the related `exit=5` bucket

## Remaining cheap / medium candidates from this bucket

Do not count these as solved by this batch:

- `fdatasync01`: missing `fdatasync` syscall semantics (`ENOSYS` in baseline/current). Likely ★ if mapped to file flush/no-op with fd validation.
- `fstatat01`: mixed path/flag behavior. Likely ●.
- `fchownat*` / `lchown*`: ownership and symlink ownership semantics. Likely ●.
- `fcntl01*`: fd allocation / flag expectation mismatch. Likely ●.
- `fcntl11*`, `fcntl14*`, `fcntl17*`...`fcntl32*`: record locks / leases. Likely ✗ or high ●.
- `ftest*`: truncate plus non-empty `rmdir` behavior. Likely ●, with broad filesystem semantics risk.
