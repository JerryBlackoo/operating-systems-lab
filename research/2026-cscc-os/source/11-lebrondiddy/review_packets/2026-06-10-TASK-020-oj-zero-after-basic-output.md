# TASK-020 OJ Zero After Basic Output

Context:
- OJ submission after GitLab `main` reached `dc9f1cb718a4c6a493d2edceb5d62971e8c385e9`.
- OJ page shows last submitted source time `2026-06-10 22:30:54 +08:00`, start
  `2026-06-10 22:39:16.248922 +08:00`, finish
  `2026-06-10 22:41:39.805052 +08:00`, total score `0.0`.
- Downloaded source archive from the OJ page has git HEAD
  `dc9f1cb718a4c6a493d2edceb5d62971e8c385e9`, so the TASK-019 signal fix was
  present in the judged source.

Important correction:
- A stale/duplicate page read initially appeared to show the old high-address
  panic. A fresh DOM extraction of the current `content101` and `content102`
  blocks shows no panic.

Observed raw output:
- RISC-V output lines 93-160 contain:
  - `#### OS COMP TEST GROUP START basic-glibc ####`
  - one `Testing <case> :` line for every basic case through `Testing yield :`
  - `#### OS COMP TEST GROUP END basic-glibc ####`
  - `#### OS COMP TEST GROUP START basic-musl ####`
  - the same `Testing <case> :` sequence
  - `#### OS COMP TEST GROUP END basic-musl ####`
- LoongArch output lines 34-101 contain the same basic-glibc/basic-musl marker
  and `Testing <case> :` sequences.
- No `panic`, `kernel page error`, `PASS`, `FAIL`, `success`, or per-case result
  line appears in the downloaded raw output.

Score symptom:
- In the OJ table, every basic case has `pass=0` while `all` is nonzero, for
  both RV/LA and glibc/musl.
- This suggests the judge detected test groups/cases but did not detect any
  successful case result.

Likely investigation direction:
- Determine whether the shell scripts are only printing their `Testing ... :`
  prologue while the real test binaries fail to execute, return nonzero, or have
  output swallowed.
- Compare official test script expected success strings against what ByteOS
  currently prints.
- Do not fake pass markers. The fix must execute real test binaries and preserve
  real success/failure output in the judge-visible console stream.
