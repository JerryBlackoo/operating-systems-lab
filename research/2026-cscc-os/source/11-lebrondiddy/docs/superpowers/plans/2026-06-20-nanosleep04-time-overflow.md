# Nanosleep04 Time Overflow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Protect the OJ1508 `main` baseline, fix the `nanosleep04` time overflow panic on the wip branch, and record honest verification evidence.

**Architecture:** Restore `main` with a revert commit so kernel behavior matches `e206a19` while retaining score documentation. Investigate the panic on `wip/time-nanosleep-overflow-20260620`, add narrow timespec/deadline validation, and verify with build, quick, targeted, focused, and full/OJ-like evidence.

**Tech Stack:** Rust `no_std` kernel, contest Docker image `zhouzhouyi/os-contest:20260510`, QEMU RV/LA harness scripts, official staged 4G sdcard images on `yxzhang@10.102.35.44`.

---

### Task 1: Protect Main

**Files:**
- Modify: `kernel/kernel/src/syscall/fd.rs`
- Create: `review_packets/2026-06-20-main-restored-oj1508.md`

- [ ] Confirm local and `origin/main` point at `ea02955`.
- [ ] Create branch `wip/time-nanosleep-overflow-20260620` at `ea02955`.
- [ ] Run `git revert -m 1 1577efa --no-edit` on `main`.
- [ ] Confirm `git diff e206a19..HEAD -- ':!PLAN.md' ':!docs/**' ':!review_packets/**'` is empty.
- [ ] Run `make -B all` and basic/busybox gates on the remote contest environment.
- [ ] Push `main`, tag `oj1508-restored-20260620`, and push the tag.
- [ ] Write the main restoration packet with command output, commit ids, and gate summaries.

### Task 2: Root Cause Time Panic

**Files:**
- Read: `review_packets/2026-06-19-post-unlinkat-full-calibration.md`
- Read/modify: `kernel/kernel/src/syscall/time.rs`
- Read: `kernel/crates/libc-types/src/types.rs`
- Read/modify if needed: `kernel/kernel/src/syscall/fd.rs`

- [ ] Inspect all `Duration` construction and addition paths fed by user `timespec` data.
- [ ] Inspect ppoll, pselect, epoll, and wait deadline paths for the same pattern.
- [ ] Reproduce or cite full/OJ-like evidence showing the panic at `nanosleep04`.
- [ ] State the exact root cause before editing production code.

### Task 3: Regression Test

**Files:**
- Create or modify the narrowest available Rust test for timespec conversion.

- [ ] Add a failing test for invalid/overflow `timespec` conversion.
- [ ] Run the specific test and capture the expected failure.

### Task 4: Minimal Fix

**Files:**
- Modify: `kernel/kernel/src/syscall/time.rs`
- Modify if same user-controlled pattern exists: `kernel/kernel/src/syscall/fd.rs`

- [ ] Add one helper that rejects `nsec >= 1_000_000_000`.
- [ ] Reject negative `time_t` encodings currently represented as oversized unsigned seconds unless Linux semantics require a different result.
- [ ] Replace user-controlled naked `current_time() + duration` with checked or saturating deadline construction.
- [ ] Keep the change scoped; do not alter LTP runner or harness semantics.
- [ ] Remove diagnostic logs before final commit.

### Task 5: Verification And Packet

**Files:**
- Create: `review_packets/2026-06-20-nanosleep04-time-overflow-fix.md`

- [ ] Run clean build: `rm -rf kernel/target && make -B all`.
- [ ] Confirm `kernel-rv` is RVC soft-float ABI and `kernel-la` is LoongArch.
- [ ] Run basic-4 quick gate.
- [ ] Run busybox-musl targeted gate with RV `55/55` and LA `54/55` with only the known kill line.
- [ ] Run focused `nanosleep04`/neighbor evidence proving no kernel panic.
- [ ] Run full/OJ-like LA evidence proving the panic is gone and RV evidence showing `ltp-musl` is not still an entire zero column.
- [ ] Commit fix and packet, push the wip branch, and state whether it is safe to merge.
