# OSKernel2025-X

Contest kernel workspace for the 2026 OS kernel implementation contest.

The kernel baseline is imported from `Byte-OS/ByteOS` at commit
`06171525001d73f8660c9fb23b11690b32cefe3d` under `kernel/`.

Attribution and license notes are recorded in `NOTICE.md`. The original ByteOS
README is preserved at `kernel/README.md`.

Current M1 target: wire the imported baseline so top-level `make all` produces
`kernel-rv` and `kernel-la` for the official image
`zhouzhouyi/os-contest:20260510`.
