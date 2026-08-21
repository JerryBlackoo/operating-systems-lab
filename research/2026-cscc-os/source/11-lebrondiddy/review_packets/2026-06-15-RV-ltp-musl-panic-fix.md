# RV LTP musl Panic Fix

## Scope

This packet covers the P1 RISC-V `ltp-musl` kernel panic that occurred after
`ltp-glibc` completed and `ltp-musl` reached the early `add_ipv6addr` region.

## Symptom

Historical failing signature:

```text
kernel page error: 0xffffffcd7d155000 sepc: 0xffffffc080268fb4
```

The same failure family was reproduced in the long diagnostic run with a nearby
post-link address:

```text
kernel page error: 0xffffffcd7d155000 sepc: 0xffffffc0802690ea ra: 0xffffffc08021f340
```

`addr2line` / `objdump` against the RISC-V `kernel-rv` symbols locate these as:

```text
0xffffffc080268fb4 -> jbd_recover
0xffffffc0802690ea -> jbd_recover
0xffffffc08021f340 -> <alloc::collections::btree::map::BTreeMap<K,V,A> as core::ops::drop::Drop>::drop
```

The disassembly confirms that both `sepc` addresses are inside the ext4 journal
recovery routine:

```text
ffffffc080268ee8 T jbd_recover
ffffffc080269184 T jbd_journal_start
```

The observed control context is therefore: `ltp-musl/add_ipv6addr` reaches task
cleanup, Rust drops a `BTreeMap`, and the cleanup path enters `jbd_recover`,
where a corrupted kernel virtual address faults.

The most useful diagnostic run found a bogus PTE containing ASCII data:

```text
pt-release suspicious root=0x9032e000 level=2 idx=*/1/14 pte=0x545359535f455641 addr=0x654d7d155000
```

`0x545359535f455641` is little-endian text from `HAVE_SYSTEMCTL`, a string in
`ltp/testcases/bin/daemonlib.sh`. That showed that user file/script contents
had reached a page-table page.

## Root Cause

`MemArea::sub()` mishandled the middle-split case. It removed the right half of
the trackers from the original area before moving them into the returned area,
which dropped those trackers and released their physical frames while the page
table still retained the corresponding user mappings.

Those stale user mappings could then continue to write through a virtual address
that pointed at a frame already returned to the allocator. When that frame was
reused as a page-table page, user data polluted PTE memory and later crashed
page-table release.

## Fix

- `PageAllocImpl::alloc()` clears newly allocated page-table pages.
- `PageAllocImpl::dealloc()` clears page-table pages before returning the frame
  to the allocator.
- `MemArea::sub()` now:
  - unmaps pages in the removed middle range,
  - moves right-half trackers into the returned `MemArea`,
  - keeps left-half trackers in the original `MemArea`,
  - preserves the correct file offset for split and prefix-trim cases.

## Fresh Verification

All commands below ran on the remote WSL host in the official
`zhouzhouyi/os-contest:20260510` image after a clean build.

### Clean Build

Log:

```text
.codex-logs/clean-build-pte-fix-20260615-192604.log
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
.codex-logs/rv-basic-only-pte-clean-20260615-192646.log
.codex-logs/la-basic-only-pte-clean-20260615-192657.log
```

Results:

```text
PASS: RISC-V basic-glibc,basic-musl markers and real basic case output found
PASS: LoongArch basic-glibc,basic-musl markers and real basic case output found
```

### Targeted RV LTP Repro

Log:

```text
.codex-logs/rv-ltp-pte-clean-targeted-20260615-192733.log
```

Result:

```text
#### OS COMP TEST GROUP END ltp-musl ####
initproc watchdog: group ltp-musl finished in 47s (truncated)
PASS: RISC-V ltp-glibc,ltp-musl markers and real basic case output found
```

The run executed both `ltp-glibc` and `ltp-musl` through `add_ipv6addr` and
continued through `add_key01` to `add_key05` without `panic`, `page error`,
`kernel page error`, `pt-release suspicious`, or `page-alloc suspicious`.

Fresh recheck after the later anonymous-`mmap()` fd fix:

```text
.codex-logs/rv-ltp-pte-clean-targeted-after-anon-mmap-20260616.log
```

Result:

```text
RUN LTP CASE add_ipv6addr
RUN LTP CASE add_key01
RUN LTP CASE add_key02
RUN LTP CASE add_key03
RUN LTP CASE add_key04
RUN LTP CASE add_key05
initproc ltp: attempted 20 of 23 cases, 8 returned 0
#### OS COMP TEST GROUP END ltp-musl ####
PASS: RISC-V ltp-glibc,ltp-musl markers and real basic case output found
```

A direct scan of that fresh log found no `panic`, `page error`,
`pt-release suspicious`, or `page-alloc suspicious` signal.

## Notes

The targeted LTP image intentionally kept only the early LTP binaries needed to
cross the historical panic point. It did not modify the harness and did not count
garbage TBROK as progress. The purpose of this run was survivability over the
known panic point; wider TPASS improvement remains part of the later P1 LTP
blocker work.
