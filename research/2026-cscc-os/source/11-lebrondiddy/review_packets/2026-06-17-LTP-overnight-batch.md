# 2026-06-17 LTP overnight batch

## Scope

This packet summarizes the overnight `wip/ltp-reach-20260616` LTP batch.

It reports targeted mini-bucket deltas, not a fresh full 6600s baseline. Every
code batch below used focused `la-musl` LTP RED/GREEN evidence plus a basic-4
gate unless noted in its packet.

Baseline reference remains:

```text
.codex-logs/ltp-baseline-la-musl-6600-20260616.log
la-musl: attempted 2820 of 2820 cases, 173 returned 0
```

## Commits

```text
d955936 LTP batch: add getsid syscall
be690a3 LTP batch: reject non-empty ramfs rmdir
a51995c LTP batch: add getpriority syscall
26c59a4 LTP batch: add fd-based chdir
942b498 LTP batch: validate fchownat lookup flags
6446101 LTP batch: fix fcntl dupfd and setfl flags
20f88e9 LTP batch: fix fstatat statx lookup flags
730951a LTP batch: add path truncate syscall
48a21c6 LTP batch: add fdatasync fd validation
5baac57 LTP batch: fix fallocate and rmdir cleanup
```

## Targeted Delta

| Batch | Commit | Focus | RED -> GREEN | Net |
| --- | --- | --- | --- | --- |
| fallocate/rmdir cleanup | `5baac57` | `fallocate01`, `fallocate02` | `0 -> 2 returned0` | `+2` |
| fdatasync | `48a21c6` | `fdatasync01`, `fdatasync02`; `fdatasync03` remains device-blocked | `0 -> 2 returned0` in a 3-case slice | `+2` |
| truncate | `730951a` | `ftest01/03/05/07`, `truncate02*`, `ftruncate01*`; `truncate03*` and `ftruncate03*` remain | `2 -> 8 returned0` in a 12-case slice | `+6` |
| fstatat/statx | `20f88e9` | `fstatat01` | `0 -> 1 returned0` | `+1` |
| fcntl | `6446101` | `fcntl01`, `fcntl01_64`, `fcntl02`, `fcntl02_64` | `0 -> 4 returned0` | `+4` |
| fchownat | `942b498` | `fchownat01` | `0 -> 1 returned0` | `+1` |
| fchdir | `26c59a4` | `fchdir01`, `fchdir02`; `fchdir03` remains permission-check work | `0 -> 2 returned0` in a 3-case slice | `+2` |
| getpriority | `a51995c` | `getpriority01`, `getpriority02` | `0 -> 2 returned0` | `+2` |
| getsid | `d955936` | `getsid01`, `getsid02` | `0 -> 2 returned0` | `+2` |
| ramfs rmdir | `be690a3` | `ftest02`, `ftest06` | `0 -> 2 returned0` | `+2` |
| getpgid | current batch | `getpgid01`, `getpgid02` | `0 -> 2 returned0` | `+2` |
| chown metadata | current batch | `chown05`, `fchown05`, `lchown01`, `fchownat02`, `chown03`, `fchown03` | `0 -> 6 returned0` | `+6` |
| lchown02 | current batch | `lchown02` errno/permission matrix | `0 -> 1 returned0` | `+1` |
| fchdir03 | current batch | `fchdir03` directory search permission after `seteuid(nobody)` | `0 -> 1 returned0` | `+1` |
| truncate03 | current batch | `truncate03`, `truncate03_64` path errno matrix | `0 -> 2 returned0` | `+2` |
| fcntl07 | current batch | `fcntl07`, `fcntl07_64` close-on-exec through exec | `0 -> 2 returned0` | `+2` |

Targeted mini-bucket total:

```text
+38 returned0 across focused slices
```

Additional no-code finding:

```text
.codex-logs/la-musl-ftest04-08-red-20260617.log
ftest04/ftest08 current tree: attempted 2, 2 returned 0
```

This removes `ftest04/08` from the immediate rmdir queue, but it did not create
a separate code delta.

## Verification Index

Each batch packet records its own RED/GREEN logs and basic-4 gate:

- `review_packets/2026-06-17-LTP-fdatasync-batch.md`
- `review_packets/2026-06-17-LTP-truncate-batch.md`
- `review_packets/2026-06-17-LTP-fstatat-batch.md`
- `review_packets/2026-06-17-LTP-fcntl-batch.md`
- `review_packets/2026-06-17-LTP-fchownat-batch.md`
- `review_packets/2026-06-17-LTP-fchdir-batch.md`
- `review_packets/2026-06-17-LTP-getpriority-batch.md`
- `review_packets/2026-06-17-LTP-getsid-batch.md`
- `review_packets/2026-06-17-LTP-ramfs-rmdir-batch.md`
- `review_packets/2026-06-17-LTP-getpgid-batch.md`
- `review_packets/2026-06-17-LTP-chown-metadata-batch.md`
- `review_packets/2026-06-17-LTP-lchown02-batch.md`
- `review_packets/2026-06-17-LTP-fchdir03-batch.md`
- `review_packets/2026-06-17-LTP-truncate03-batch.md`
- `review_packets/2026-06-17-LTP-fcntl07-batch.md`
- `review_packets/2026-06-17-LTP-ftruncate03-failed-attempt.md`

Latest fresh verification after `fcntl07`:

```text
.codex-logs/cargo-fmt-fcntl07-20260617.log
.codex-logs/force-make-fcntl07-20260617.log
.codex-logs/la-musl-fcntl07-green-20260617.log
.codex-logs/la-musl-fcntl01-02-07-regress-20260617.log
.codex-logs/basic-rv-basic-only-fcntl07-20260617.log
.codex-logs/basic-la-basic-only-fcntl07-20260617.log
```

Build ABI evidence:

```text
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, not stripped
kernel-la: ELF 64-bit LSB executable, LoongArch, version 1 (GNU/Linux), statically linked, not stripped
Flags: 0x1, RVC, soft-float ABI
```

## Remaining Queue

Cheap / contained:

- Small errno-matrix syscall cases only when focused RED proves a narrow missing dispatch or flag check.

Medium:

- `ftruncate03*`: fd permission/socket/space errno matrix. Two speculative
  attempts did not improve the focused run; the latest fd/type/limit check
  patch is stashed as `failed-ftruncate03-wip-20260617` and documented in
  `review_packets/2026-06-17-LTP-ftruncate03-failed-attempt.md`.

Expensive / skip unless strategically necessary:

- `fdatasync03` and `lchown03*`: block-device or device-acquisition setup.
- `fcntl11*`, `fcntl14*`, `fcntl17*`..`fcntl32*`: record locks and leases.
- cgroup / namespace families: `cgroup*`, `cpuacct*`, `cpuctl*`, `userns*`, `unshare*`.
- network stress/script families: `gre*.sh`, `icmp*`, `tcp*`, `udp*`.
- exit-127 bucket: broad executable/script/interpreter asset classification, not a cheap syscall patch.

## Notes For Next Session

- Stay on `wip/ltp-reach-20260616`.
- `PLAN.md` is intentionally left unstaged and was not edited by this batch.
- OJ submission remains human-only.
- Keep using the official Docker image on the WSL host for trusted builds/tests.
- For basic-only gates, keep using disposable cropped images; `EXPECTED_GROUPS`
  alone does not stop later groups.
