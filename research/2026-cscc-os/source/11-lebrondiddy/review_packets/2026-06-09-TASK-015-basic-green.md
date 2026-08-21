# TASK-015 Boundary Review

- Run: `TASK-015-20260609-193436`
- Local log: `/Users/yuxuanzhang/Desktop/2025-2026-2/操作系统/内核实现赛/.hermes/runs/TASK-015-20260609-193436/codex.log`
- Remote log: `/home/lenovo/oskernel2025-x/.hermes/runs/TASK-015-20260609-193436/codex.log`
- Remote diff copy: `.hermes/runs/TASK-015-20260609-193436/remote.diff.patch`

## Boundary Result

TASK-015 is accepted. In the official image with no network, `make all`
produced ELF `kernel-rv` and `kernel-la`; both
`TIMEOUT_SECONDS=120 ./harness/run-rv.sh` and
`TIMEOUT_SECONDS=120 ./harness/run-la.sh` exited 0 for `basic`.

Root cause: the LoongArch `basic/fstat` binary uses Linux `statx(fd, "",
AT_EMPTY_PATH, STATX_BASIC_STATS, ...)` syscall 291. The kernel lacked `statx`,
so LoongArch returned an unsupported syscall result. TASK-015 added the needed
`statx` compatibility path without weakening harness checks.

## Follow-Up

TASK-012 is ready for a fresh explicit green-gate check. Do not submit unless
the accepted changes are committed and pushed to the GitLab repository link the
judge will fetch.
