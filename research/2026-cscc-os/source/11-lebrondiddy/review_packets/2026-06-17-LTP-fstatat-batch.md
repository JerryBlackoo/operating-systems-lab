# 2026-06-17 LTP fstatat batch

## Scope

Targeted `la-musl` LTP `fstatat01`.

Root cause:

- `fstatat()` from the LoongArch musl binary reaches the stat compatibility path using `statx`-style flags, not only the direct `fstatat` syscall path.
- The kernel accepted unknown stat lookup flags instead of returning `EINVAL`.
- Relative lookup with a non-directory `dirfd` fell through to path resolution and returned `ENOENT`; Linux expects `ENOTDIR`.
- Musl includes `AT_NO_AUTOMOUNT` in the statx-backed `fstatat()` path, so `AT_NO_AUTOMOUNT` must be treated as a valid stat lookup flag.

## Change

- Added a shared stat lookup flag mask:
  - `AT_EMPTY_PATH`
  - `AT_NO_AUTOMOUNT`
  - `AT_SYMLINK_NOFOLLOW`
- Applied the mask to both `sys_fstatat` and `sys_statx`.
- Added relative-path `dirfd` directory validation to both paths, returning `ENOTDIR` when `dirfd` is an existing non-directory fd.
- Preserved the existing `AT_EMPTY_PATH` behavior used by LoongArch basic stat tests.

## Evidence

RED:

```text
.codex-logs/la-musl-fstatat01-red-20260617.log
RUN LTP CASE 000_fstatat01
subtest 3: TFAIL, got ENOENT instead of ENOTDIR
subtest 5: TFAIL, got SUCCESS instead of EINVAL
FAIL LTP CASE 000_fstatat01 : 1
initproc ltp: attempted 1 of 2821 cases, 0 returned 0
```

GREEN:

```text
.codex-logs/la-musl-fstatat01-green3-20260617.log
RUN LTP CASE 000_fstatat01
subtest 1: TPASS SUCCESS
subtest 2: TPASS SUCCESS
subtest 3: TPASS ENOTDIR
subtest 4: TPASS EBADF
subtest 5: TPASS EINVAL
subtest 6: TPASS SUCCESS
FAIL LTP CASE 000_fstatat01 : 0
initproc ltp: attempted 1 of 2821 cases, 1 returned 0
```

Build and ABI:

```text
.codex-logs/cargo-fmt-fstatat-statx2-20260617.log
.codex-logs/force-make-fstatat-statx2-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic-4 gate:

```text
.codex-logs/basic4-rv-basic-only-fstatat-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic4-la-basic-only-fstatat-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Debug Notes

Two non-final attempts were intentionally discarded as evidence:

- `force-make-fstatat2-20260617.log`: compile failed because the `AT_EMPTY_PATH` fd branch mixed `File` and `Arc<File>`.
- `la-musl-fstatat01-green2-20260617.log`: all subtests returned `EINVAL` because the initial statx flag mask omitted musl's valid `AT_NO_AUTOMOUNT` bit.
