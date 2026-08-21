# 2026-06-17 LTP ramfs rmdir batch

## Scope

Targeted a narrow `la-musl` LTP slice:

- `ftest02`
- `ftest06`

Root cause:

- LTP creates non-empty directories under `/tmp`, which is mounted as RamFS.
- `RamDir::rmdir()` removed a matching child directory without checking whether
  that directory still had children.
- Linux `rmdir(2)` must reject non-empty directories with `ENOTEMPTY`.

## Change

- Updated RamFS `rmdir()` to:
  - return `ENOENT` when the named child directory does not exist;
  - inspect the target directory's `children`;
  - return `ENOTEMPTY` when it is non-empty;
  - remove the child only when the directory is empty.

Out of scope:

- Ext4-backed `rmdir()` semantics.
- Directory permission/search checks.

## Evidence

RED:

```text
.codex-logs/la-musl-ftest02-06-red-20260617.log
ftest02.c:346: Test[0]: rmdir of non-empty ... succeeds!
ftest06.c:364: Test[0]: rmdir of non-empty ... succeeds!
initproc ltp: attempted 2 of 2822 cases, 0 returned 0
000_ftest02 : 1
001_ftest06 : 1
```

GREEN:

```text
.codex-logs/la-musl-ftest02-06-green-20260617.log
ftest02: TPASS: Test passed in fork-wait part.
ftest06: TPASS: Test passed.
initproc ltp: attempted 2 of 2822 cases, 2 returned 0
000_ftest02 : 0
001_ftest06 : 0
```

Delta:

```text
2 targeted cases: 0 -> 2 returned 0 (+2)
```

Format/build and ABI:

```text
.codex-logs/cargo-fmt-ramfs-rmdir-20260617.log
.codex-logs/force-make-ramfs-rmdir-20260617.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

Basic-4 gate:

```text
.codex-logs/basic4-rv-basic-only-ramfs-rmdir-20260617.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic4-la-basic-only-ramfs-rmdir-20260617.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

## Remaining Work

- Broader RamFS directory errno behavior should still be case-driven.
- Ext4 `rmdir()` still delegates through `unlink()` and has not been audited in this batch.
