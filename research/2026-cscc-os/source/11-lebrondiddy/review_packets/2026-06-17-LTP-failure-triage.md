# 2026-06-17 LTP failure triage

Start timestamp:

```text
Wed Jun 17 00:08:46 CST 2026
```

Baseline source:

- `.codex-logs/ltp-baseline-la-musl-6600-20260616.log`
- `.codex-logs/ltp-baseline-la-musl-6600-20260616.summary`

Baseline summary:

```text
la-musl: attempted 2820 of 2820 cases, 173 returned 0
```

Exit-code buckets:

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

## Failure table

| Case / bucket | Failure reason | Root-cause cluster | Cost | Status |
| --- | --- | --- | --- | --- |
| `fallocate01`, `fallocate02` | baseline exit 5; `fallocate()` did not extend files / returned wrong errno; cleanup `tst_rmdir` warning | `fallocate` semantics + `unlinkat/rmdir` cleanup dispatch | ★ | fixed in `5baac57`, targeted `0 -> 2 returned0` |
| `fdatasync01`, `fdatasync02` | `fdatasync()` returned `ENOSYS`; invalid fd expected `EBADF`, special fd expected `EINVAL` | missing `fdatasync` syscall dispatch and fd/type errno matrix | ★ | fixed in current fdatasync batch, targeted `0 -> 2 returned0` |
| `fdatasync03` | `TBROK: Failed to acquire device` | missing/free block device or device setup for LTP | ✗ | not part of fdatasync syscall batch |
| `fstatat01` | invalid flags expected `EINVAL`; relative path with non-directory `dirfd` expected `ENOTDIR`; musl reaches this through statx-style flags | `fstatat` / `statx` lookup flag and dirfd semantics | ★ | fixed in current fstatat batch, targeted `0 -> 1 returned0` |
| `fchownat01` | `fchownat()` ignored `dirfd`, path, and flags because dispatcher routed it to a no-arg no-op | `fchownat` parameter validation / lookup errno matrix | ★ | fixed in current fchownat batch, targeted `0 -> 1 returned0` |
| `fchownat02`, `lchown01`, `chown05`, `fchown05`, `chown03`, `fchown03` | ownership/symlink ownership expectations and setuid/setgid clearing | RamFS chown/chmod metadata model | ● | fixed in chown metadata batch, targeted `0 -> 6 returned0` |
| `lchown02` | non-root ownership change, missing directory search permission, non-directory path prefix, and empty-path errno mismatches | chown-family permission/path errno matrix | ● | fixed in lchown02 batch, targeted `0 -> 1 returned0` |
| `lchown03*` | remaining lchown device cases | lchown device setup | ✗ | remaining; device-acquisition flavored |
| `fchdir01`, `fchdir02` | `fchdir()` returned `ENOSYS`; invalid fd expected `EBADF` | missing `fchdir` dispatch and fd target validation | ★ | fixed in current fchdir batch, targeted `0 -> 2 returned0` |
| `fchdir03` | `fchdir()` should fail with `EACCES` for a directory without search permission after seteuid | directory permission/search checks | ● | fixed in fchdir03 batch, targeted `0 -> 1 returned0`; `fchdir01`-`fchdir03` regression `3 -> 3 returned0` |
| `fcntl01*`, `fcntl02*` | `F_DUPFD` ignored minimum fd; `F_SETFL` dropped access mode bits | fd allocation and status flag semantics | ★ | fixed in current fcntl batch, targeted `0 -> 4 returned0` |
| `fcntl07*` | child exited 139 while checking close-on-exec across regular/pipe/fifo fds; `execlp()` fallback could destroy the old image before returning `ENOENT`, loader lookup used the wrong base, and exec did not close `FD_CLOEXEC` descriptors | close-on-exec / exec image switch and loader path | ● | fixed in fcntl07 batch, targeted `0 -> 2 returned0`; `fcntl01/02/07` regression `6 -> 6 returned0` |
| `fcntl11*`, `fcntl14*`, `fcntl17*`..`fcntl32*` | exit 5 | record locks / leases | ✗ / high ● | skip until higher ROI |
| `getpriority01`, `getpriority02` | `getpriority()` returned `ENOSYS`; valid current process/group/user expected nice `0`, invalid `which` expected `EINVAL`, negative `who` expected `ESRCH` | missing `getpriority` dispatch and Linux raw syscall return convention | ★ | fixed in current getpriority batch, targeted `0 -> 2 returned0` |
| `getsid01`, `getsid02` | `getsid()` returned `ENOSYS`; current parent/child session ids should match, unused pid should return `ESRCH` | missing `getsid` dispatch; no explicit session field, but current parent-chain `setsid` model is enough for this slice | ★ | fixed in current getsid batch, targeted `0 -> 2 returned0` |
| `getpgid01`, `getpgid02` | `getpgid(1)` disagreed with `/proc/1/stat`; invalid negative and unused pids succeeded | `getpgid` ignored pid argument and returned constant pgid `0` | ★ | fixed in current getpgid batch, targeted `0 -> 2 returned0` |
| `ftest01`, `ftest03`, `ftest05`, `ftest07`, `truncate02*` | path-based `truncate()` returned `ENOSYS` | missing `Sysno::truncate` dispatch and path truncate handler | ★ | fixed in current truncate batch, targeted `2 -> 8 returned0` across 12-case slice |
| `truncate03*` | path errno matrix still mismatched after `truncate()` dispatch | truncate path permission/prefix/limit errno semantics | ● | fixed in truncate03 batch, targeted `0 -> 2 returned0`; 10-case truncate regression `10 -> 10 returned0` |
| `ftruncate03*` | fd permission/socket/space errno matrix still mismatched | ftruncate fd errno semantics | ● | attempted fd/type/limit patch still `0 -> 0 returned0`; stashed as `failed-ftruncate03-wip-20260617`; next pass needs test-source-level triage, not replaying the same checks |
| `ftest02`, `ftest06` | non-empty `rmdir` succeeded under `/tmp` RamFS | RamFS `rmdir()` did not reject non-empty child directories | ★ | fixed in current ramfs-rmdir batch, targeted `0 -> 2 returned0` |
| `ftest04`, `ftest08` | prior baseline failure no longer reproduces in focused current-tree run | already green on current tree | ★ | no code change; focused `la-musl` run returned `2 -> 2 returned0` |
| exit-127 bucket | many `unknown command: ltp/testcases/bin/...` | executable format / script / missing interpreter / unsupported test assets depending case | ✗ / mixed | not cheap; classify sub-buckets later |
| cgroup / namespace cases | exit 1/2/32/36/127 across `cgroup*`, `cpuacct*`, `cpuctl*`, `userns*`, `unshare*` | cgroup / namespace unsupported | ✗ | red-line skip for overnight batch |
| network stress/script family | `gre*.sh`, `icmp*`, `tcp*`, `udp*`, many 127/139 | network stack and script execution path | ✗ | skip unless reach wall requires quarantine/root fix |
| timeout bucket | exit 124 (`abort01`, math generators, fork/network stress, etc.) | per-case timeout or task cleanup budget | mixed | only revisit if high multiplier and reproducible |

## Next cheap / medium queue

1. `ftruncate03*` errno matrix only after reading the LTP source/setup; the latest fd/type/limit patch did not improve the focused run.
