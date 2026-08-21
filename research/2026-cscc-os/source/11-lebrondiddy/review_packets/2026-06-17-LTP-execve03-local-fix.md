# 2026-06-17 LTP execve03 local fix checkpoint

## Scope

Branch under work:

```text
wip/ltp-reach-20260616
```

Goal from the active task:

- remove the hard kernel panic in glibc LTP `execve03`;
- preserve the musl LTP gains from the WIP branch;
- verify with clean official-image build, basic-4, and four-quadrant
  judge-budget LTP before commit.

This packet is a local checkpoint only. The remote WSL execution target is still
unreachable on TCP/22, so the acceptance gates have not been run yet.

## Root Cause

Prior M3 evidence:

```text
review_packets/2026-06-17-LTP-merge-readiness.md
.codex-logs/m3-ltp-rv-glibc-6600-20260617.log
.codex-logs/m3-ltp-la-glibc-6600-20260617.log
```

Both glibc quadrants reached `execve03` and then panicked:

```text
RUN LTP CASE execve03
[Core 0] [kernel/src/tasks/exec.rs:218]
[Core 0] panic: 'index out of bounds: the len is 0 but the index is 0'
```

The root cause was in `kernel/kernel/src/tasks/exec.rs`: an empty executable
file produced `file_size == 0`, `frame_alloc_much(0)` returned an empty frame
vector, and the loader indexed `[0]`.

## Local Change

`exec_with_process()` now:

- returns `ENOEXEC` before frame allocation when `file_size == 0`;
- returns `ENOMEM` if ELF-file buffer allocation fails;
- avoids indexing the frame vector directly;
- returns `ENOEXEC` on short reads and invalid ELF magic;
- removes the busybox applet `unwrap()` on the open-error fallback;
- validates loadable ELF segments before replacing the old address space;
- returns `ENOEXEC` for malformed/empty load segments instead of panicking;
- returns `ENOMEM` if mapping a validated load segment fails;
- removes debug `assert_eq!` checks from the segment copy path.

## Local Static Evidence

Static panic-pattern audit:

```sh
rg -n "\.unwrap\(\)|\.expect\(|assert_eq!|assert!|as_ref\(\)\.unwrap\(\)\[0\]|frame_ppn\[0\]|get_type\(\)\.unwrap\(|busybox_applet\(&exec_path\)\.unwrap|assert_eq!\(\s*elf_header" kernel/kernel/src/tasks/exec.rs
```

Result: no matches.

Whitespace check:

```sh
git diff --check
```

Result: exit 0.

Local Rust build was not available on the Mac:

```text
rustc: command not found
cargo: command not found
```

This is not an acceptance substitute; official-image verification must run on
remote WSL.

## 2026-06-17 continuation check

Current local branch:

```text
wip/ltp-reach-20260616
```

Current uncommitted files observed during this continuation:

```text
REMOTE_EXECUTION.md
docs/PLAYBOOK.md
kernel/kernel/src/tasks/exec.rs
review_packets/2026-06-17-LTP-execve03-local-fix.md
```

Local static checks rerun:

```sh
git diff --check
```

Result: exit 0.

Panic-pattern audit rerun:

```sh
rg -n "\.unwrap\(\)|\.expect\(|assert_eq!|assert!|as_ref\(\)\.unwrap\(\)\[0\]|frame_ppn\[0\]|get_type\(\)\.unwrap\(|busybox_applet\(&exec_path\)\.unwrap|assert_eq!\(\s*elf_header" kernel/kernel/src/tasks/exec.rs
```

Result: no matches.

Code review notes:

- `execve03` panic source is still covered: empty files now return `ENOEXEC`
  before frame allocation.
- ELF segment validation is done before `memset.clear()` and page-table switch,
  so malformed ELF input should not destroy the old address space before
  returning an error.
- Segment mapping still needs official-image execution to prove no boot/runtime
  regression; local static review is not enough.

## Remote Status

Current intended target:

```text
ssh -p 22 lenovo@100.92.143.96
```

Latest Mac-side checks:

```text
tailscale peerapi 100.92.143.96: reachable
tailscale icmp 100.92.143.96: reachable
100.92.143.96:22: timed out
100.92.143.96:2222: TCP connects but SSH resets
192.168.1.3:22: TCP connects but SSH banner exchange times out
wsl-4070: Tailscale node offline
Mutagen oskernel-sync: paused / beta disconnected
```

Conclusion: the Tailscale Windows node is alive, but Windows-to-WSL SSH or the
Windows firewall/portproxy path for TCP/22 still needs repair. The repair
checklist is recorded in `REMOTE_EXECUTION.md`.

Continuation probe:

```text
ssh -p 22 lenovo@100.92.143.96: timed out
ssh -p 2222 lenovo@100.92.143.96: SSH reset during key exchange
ssh -p 22 lenovo@192.168.1.3: banner exchange timed out
ssh -p 2222 lenovo@192.168.1.3: banner exchange timed out
mutagen oskernel-sync: alpha connected, beta disconnected / connecting
```

No official-image build, basic-4, or LTP rerun was started in this continuation.

## Still Required

Do not commit this as complete until all acceptance evidence exists:

- remote official-image clean build:
  `rm -rf kernel/target kernel-rv kernel-la kernel/mount.img; make -B all`;
- `file kernel-rv` confirms `RVC, soft-float ABI`;
- basic-4 is green with `TIMEOUT_SECONDS >= 240`;
- four-quadrant LTP with `TIMEOUT_SECONDS=6600` and `STALL_TIMEOUT_SECONDS=600`;
- no `execve03` kernel panic in either glibc quadrant;
- returned0 thresholds:
  - `rv-glibc >= 173`;
  - `la-glibc >= 153`;
  - musl gains preserved near the M3 values;
  - aggregate four-quadrant returned0 `> 661`;
- final packet updated with real log paths and counts;
- commit made on `wip/ltp-reach-20260616`.

Do not merge `main`, push, or touch OJ from this task.

## Remote Commands To Run After SSH Recovers

From the Mac:

```sh
ssh -p 22 -o BatchMode=yes -o ConnectTimeout=8 lenovo@100.92.143.96 'echo connected; hostname; pwd'
mutagen sync resume oskernel-sync
```

Then on remote WSL:

```sh
cd /home/lenovo/oskernel2025-x
git status --short --branch
```

Clean official-image build and ABI gate:

```sh
docker run --rm --network none -e RUSTUP_TOOLCHAIN=nightly-2025-01-18 \
  -v /home/lenovo/oskernel2025-x:/workspace -w /workspace zhouzhouyi/os-contest:20260510 \
  bash -lc 'rm -rf kernel/target kernel-rv kernel-la kernel/mount.img; make -B all; \
            file kernel-rv kernel-la; readelf -h kernel-rv | grep -i flags'
```

Basic-4 gate:

```sh
docker run --rm --network none \
  -v /home/lenovo/oskernel2025-x:/workspace -w /workspace \
  -v /home/lenovo/Downloads/oscomp-sdcard:/oj zhouzhouyi/os-contest:20260510 \
  bash -lc 'TIMEOUT_SECONDS=240 TEST_IMAGE=/oj/sdcard-rv.img TEST_IMAGE_LAYOUT=nested \
            EXPECTED_GROUPS=basic-glibc,basic-musl bash harness/run-rv.sh; \
            TIMEOUT_SECONDS=240 TEST_IMAGE=/oj/sdcard-la.img TEST_IMAGE_LAYOUT=nested \
            EXPECTED_GROUPS=basic-glibc,basic-musl bash harness/run-la.sh'
```

For LTP, use the existing M3 image-pruning method from
`review_packets/2026-06-17-LTP-merge-readiness.md`: fresh image per quadrant,
keep only the target `ltp_testcode.sh`, remove `.ltp_case_limit`, and run with
`TIMEOUT_SECONDS=6600 STALL_TIMEOUT_SECONDS=600`.
