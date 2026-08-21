# OJ-READY - STATUS: HOLD

Updated 2026-06-15 CST by Codex.

Do not submit current `main` to OJ yet.

TASK-024 soft-float is a real kernel improvement and is safe to keep on
`main`: RV glibc now passes the basic gate, and RV busybox/lua glibc output is
real. It does not, however, honestly beat the banked 1296.25 score.

Current verified point:
- Branch merged: `wip/TASK-024-softfloat`
- Commit: `25b437721703d74566befb6a5ed23ca69ff1e688`
- Kernel tree: `a29c4ef240eef2946d33147b02daf9b643d1b0a0`
- Basic-4 gate: PASS, including RV glibc with `TIMEOUT_SECONDS=240`
- `kernel-rv`: verified `RVC, soft-float ABI`

Full regression evidence:
- Remote: `lenovo@100.92.143.96:/home/lenovo/oskernel2025-x`
- Logs: `.hermes/full-rv.log`, `.hermes/full-la.log`
- Review packet: `review_packets/2026-06-15-TASK-024-softfloat-full-regression.md`

Why this is HOLD:
- Full regression ended with `RV_RC=5` and `LA_RC=7`.
- RV missed `ltp-musl` END after a kernel page error in `add_ipv6addr`.
- LA missed `ltp-glibc` END after a 600s harness stall at `dns-stress01-rmt.sh`.
- LTP has zero real `PASS LTP CASE` lines in this run; no garbage-overflow
  LTP credit is counted.
- Conservative score estimate remains below the banked 1296.25.

OJ submission remains human-only. Wait for a later honest score estimate that
beats 1296.25 before asking the user to submit.
