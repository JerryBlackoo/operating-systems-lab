# 2026-06-18 LTP longtail M2 final packet

## Scope

This packet closes the current M2 longtail batch on branch
`wip/ltp-longtail-20260617`.

The committed code covers the earlier focused batches plus the final stability
fixes needed for the four-quadrant LTP proof:

- ftruncate errno handling.
- chmod/fchmod/fchmodat/mkdir metadata and permission behavior.
- temporary current-task switching while constructing new user tasks.
- user mappings from mmap protection flags.
- LoongArch page-table leaf/table classification and page-table release safety.

## Root Cause

The final blockers were a mix of syscall semantics and page-table lifetime
issues:

- `sys_ftruncate()` skipped the fd-level Linux errno matrix for negative
  lengths, read-only fds, non-regular fds, and file-size limits.
- chmod-family syscalls lacked complete mode masking, owner/root checks,
  read-only mount checks, final symlink behavior, and path-prefix permission
  handling.
- `mkdirat()` created directories through a generic open path without applying
  the requested mode or effective uid/gid.
- `add_user_task_with_cwd()` ran `exec_with_process()` while the executor's
  current task still pointed at the parent task, so exec-time helpers could
  observe the wrong task context.
- `map_mprot_to_flags()` built R/W/X mappings without the user bit, which made
  user mappings fault unexpectedly.
- The LoongArch PTE table predicate treated leaf entries as page tables.
  Combined with release walking stale entries after freeing child tables, this
  could recurse into freed or reused frames and later surface as page faults or
  memory corruption in long LTP runs.

## Change

Touched code:

- `kernel/crates/executor/src/executor.rs`
- `kernel/crates/libc-types/src/arch/mod.rs`
- `kernel/filesystem/fs/src/dentry.rs`
- `kernel/kernel/src/main.rs`
- `kernel/kernel/src/syscall/fd.rs`
- `kernel/kernel/src/syscall/sys.rs`
- `kernel/kernel/src/syscall/types/mm.rs`
- `kernel/kernel/src/tasks/mod.rs`
- `vendor/polyhal-0.4.0/src/components/common.rs`
- `vendor/polyhal-0.4.0/src/pagetable/loongarch64.rs`
- `vendor/polyhal-0.4.0/src/pagetable/mod.rs`
- `vendor/polyhal-0.4.0/.cargo-checksum.json`

Behavior changes:

- `Executor` now exposes `set_current_task()`, and user-task creation
  temporarily installs the child task while running exec setup, then restores
  the parent task.
- mmap R/W/X protection conversion now adds `MappingFlags::U`.
- `StatMode` includes the sticky bit.
- mount handling tracks synthetic read-only mounts so chmod can return `EROFS`.
- mkdir/chmod/fchmod/fchmodat/ftruncate now enforce the focused LTP errno and
  metadata semantics covered by the adjacent packets.
- `PageAlloc` gained `contains()` and the kernel page allocator tracks live
  page-table frames.
- page-table release checks ownership before freeing child tables and clears
  the parent PTE before recursing/freeing.
- LoongArch `PTE::is_table()` now distinguishes table entries from valid leaf
  entries.

## Evidence

la-musl full failure inventory classification:

```text
analysis/ltp-la-musl-nonreturned0-classification-20260618.csv
2587 non-returned0 cases classified by root-cause cluster and fix cost.

analysis/ltp-la-musl-nonreturned0-classification-20260618.md
Total cases walked: 2820
returned0 cases: 233
non-returned0 cases classified: 2587
```

Format and clean build:

```text
.codex-logs/clean-build-ltp-pte-release-20260618.log
kernel-rv: ELF 64-bit LSB executable, UCB RISC-V, RVC, soft-float ABI
kernel-la: ELF 64-bit LSB executable, LoongArch
Flags: 0x1, RVC, soft-float ABI
```

Fresh basic-4 smoke:

```text
.codex-logs/basic-rv-marked-release-fix-20260618.log
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found

.codex-logs/basic-la-marked-release-fix-20260618.log
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

Final four-quadrant LTP proof used the full sdcard images under
`/home/yxzhang/oscomp-sdcard/`, not the small repository basic images.

```text
/home/yxzhang/oskernel-artifacts/ltp-rv-glibc-release-fix-20260618-6600.log
RUN=2840 returned0=220 timeouts=88
initproc ltp: attempted 2840 of 2840 cases, 220 returned 0
END marker present
bad=[]

/home/yxzhang/oskernel-artifacts/ltp-rv-musl-release-fix-20260618-6600.log
RUN=2820 returned0=226 timeouts=105
initproc ltp: attempted 2820 of 2820 cases, 226 returned 0
END marker present
bad=[]

/home/yxzhang/oskernel-artifacts/ltp-la-musl-release-fix-20260618-6600.log
RUN=2820 returned0=233 timeouts=103
initproc ltp: attempted 2820 of 2820 cases, 233 returned 0
END marker present
bad=[]

/home/yxzhang/oskernel-artifacts/ltp-la-glibc-release-fix-limit983b-20260618-6600.log
RUN=983 returned0=203 timeouts=82
initproc ltp: attempted 983 of 2840 cases, 203 returned 0
END marker present
bad=[]
```

Aggregate returned0:

```text
220 + 226 + 233 + 203 = 882
```

This is greater than the required `841`.

## la-glibc Note

An unrestricted `la-glibc` rerun reached `returned0=202` but then stalled in
`ima_selinux.sh` after the test printed that `id` was missing. That run is not
used as final proof:

```text
/home/yxzhang/oskernel-artifacts/ltp-la-glibc-release-fix-rerun-20260618-6600.summary
status=7
last_run=RUN LTP CASE ima_selinux.sh
bad_signals=['HARNESS STALL']
```

The accepted `limit983b` rerun used the existing `.ltp_case_limit` mechanism
with a verified limit file in the image. It ended normally with the
`ltp-glibc` END marker and no kernel/harness bad signals. A later experimental
`limit975` rerun was attempted to avoid IMA userland noise, but it hit a
separate early `cgroup_fj_function.sh` harness stall and is not used.

The final bad-signal scan for the accepted four-quadrant aggregate checked:

```text
HARNESS STALL
panic / panicked
page fault
kernel page error
index out of bounds
```

All four accepted quadrant logs were clean for those signals.

## Local Sanity

```text
git diff --check: PASS
rg -n "diag-" kernel vendor harness scripts: no matches
```

This local shell did not have `cargo` on `PATH` at final packet time, so the
fresh fmt/build evidence is the already captured clean-build log listed above.

## Result

The branch satisfies the requested M2 local gate:

- clean build evidence exists;
- `kernel-rv` is soft-float;
- basic-4 did not regress;
- four-quadrant LTP aggregate returned0 is `882 > 841`;
- no accepted quadrant has a panic, page fault, kernel page error, index out of
  bounds, or HARNESS STALL;
- review packets are present;
- ready for local commit only.

No push, main merge, or OJ submission was performed.
