# 2026-06-17 LTP fchownat batch

## Scope

Targeted one narrow `la-musl` LTP case:

- `fchownat01`

Root cause:

- `Sysno::fchownat` was routed together with `fchown` into a no-argument no-op, so it ignored `dirfd`, `path`, and `flags`.
- `fchownat01` expects basic Linux errno behavior for:
  - relative path through a non-directory fd -> `ENOTDIR`;
  - invalid fd -> `EBADF`;
  - unsupported flags -> `EINVAL`.

## Change

- Split syscall dispatch for `fchown` and `fchownat`.
- Added `sys_fchown(fd, uid, gid)` that validates the fd and otherwise keeps current no-op ownership semantics.
- Added `sys_fchownat(dirfd, path, uid, gid, flags)` that:
  - accepts only `AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW`;
  - reads the user path through the bounded C-string helper;
  - validates `AT_EMPTY_PATH` fd use;
  - rejects relative lookup through non-directory `dirfd` with `ENOTDIR`;
  - opens the target to preserve existing path/lookup errno behavior;
  - keeps ownership mutation as a no-op for now.

Out of scope:

- Persistent uid/gid ownership changes.
- Symlink ownership semantics for `fchownat02` / `lchown01`.

## Evidence

RED:

```text
.codex-logs/la-musl-fchownat01-red-20260617.log
initproc ltp: attempted 1 of 2821 cases, 0 returned 0
000_fchownat01 : 1
```

Key RED failures:

```text
fchownat01.c:107: fchownat() returned 0, expected -1, errno=20
fchownat01.c:107: fchownat() returned 0, expected -1, errno=9
fchownat01.c:107: fchownat() returned 0, expected -1, errno=22
```

GREEN:

```text
.codex-logs/la-musl-fchownat01-green-20260617.log
initproc ltp: attempted 1 of 2821 cases, 1 returned 0
000_fchownat01 : 0
```

Delta:

```text
1 targeted case: 0 -> 1 returned 0 (+1)
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-fchownat-remote2-20260617.log
.codex-logs/force-make-fchownat-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic-4 gate:

```text
.codex-logs/basic4-rv-basic-only-fchownat-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic4-la-basic-only-fchownat-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Remaining Work

- `fchownat02`: needs symlink ownership behavior, likely requires metadata-backed uid/gid changes.
- `lchown01` / `lchown02`: same ownership model gap, plus path errno cases.
- `lchown03*`: still device-setup blocked.
