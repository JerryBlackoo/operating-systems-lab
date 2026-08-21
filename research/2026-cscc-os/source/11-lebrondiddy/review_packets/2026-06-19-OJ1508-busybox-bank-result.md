# 2026-06-19 OJ1508 BusyBox Bank Result

Date: 2026-06-19

## Scope

This packet records the OJ result for the `oj-busybox-bank-20260619` candidate.
The submitted main line was based on `e206a190994f3456d9b024aed1e3675b2d3696e2`,
which contains the BusyBox per-child watchdog budget fix and docs-only evidence
commits. It does not contain the blocked `ae5a826` LTP manifest runner.

## OJ Result

OJ metadata:

```text
Last commit time: 2026-06-19 12:55:20
Start time: 2026-06-19 12:55:54.004563+08:00
End time: 2026-06-19 14:57:08.136063+08:00
Status: Accepted
Total: 1508.5793028047033
```

Top-level score table:

| Group | glibc-la | glibc-rv | musl-la | musl-rv | Total |
| --- | ---: | ---: | ---: | ---: | ---: |
| basic | 102 | 102 | 102 | 102 | 408 |
| busybox | 53 | 54 | 53 | 54 | 214 |
| cyclictest | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| iozone | 20.0 | 20.0 | 21.999954583337004 | 20.0 | 81.999954583337 |
| iperf | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| libcbench | 6.846545670293539 | 25.844233338421034 | 6.188446336429929 | 27.933541312494494 | 66.812766657639 |
| libctest | - | - | 29 | 190 | 219 |
| lmbench | 12.764841470130072 | 44.71068228714706 | 44.08028339034994 | 43.92183466811796 | 145.47764181574502 |
| ltp | 158 | 1239 | 0 | 1195 | 2592 |
| lua | 9 | 9 | 9 | 9 | 36 |
| netperf | 0.0 | 0.0 | 0.0 | 1.9329301009706843 | 1.9329301009706843 |

Column totals:

| Column | Score |
| --- | ---: |
| glibc-la | 361.61138714042363 |
| glibc-rv | 1494.5549156255681 |
| musl-la | 265.2686843101169 |
| musl-rv | 1643.7883060815832 |
| total | 1508.5793028047033 |

## Delta Versus A0FFF71 / OJ1389

Previous OJ result for `a0fff71`: `1389.1431752924545`.

New result: `1508.5793028047033`.

Overall delta:

```text
+119.4361275122488
```

Primary intended delta was confirmed in BusyBox:

| BusyBox column | OJ1389 | OJ1508 | Delta |
| --- | ---: | ---: | ---: |
| glibc-la | 22 | 53 | +31 |
| glibc-rv | 54 | 54 | 0 |
| musl-la | 10 | 53 | +43 |
| musl-rv | 10 | 54 | +44 |
| total | 96 | 214 | +118 |

Other groups were broadly stable. Small movement in perf/LTP groups is within
the expected judge/runtime variance for this harness.

## Decision

The BusyBox bank was successful. The current protected score is now
`1508.5793028047033`, with full basic and a near-complete BusyBox row.

The next code candidate after this result is the direct-bin-walk
`unlinkat("")` fix on `wip/la-ltp-direct-20260619`, not the blocked
`ae5a826` manifest runner.
