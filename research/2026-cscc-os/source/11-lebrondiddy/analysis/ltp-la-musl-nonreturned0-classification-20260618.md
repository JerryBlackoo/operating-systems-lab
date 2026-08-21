# 2026-06-18 la-musl non-returned0 classification

Source log:

```text
analysis/ltp-la-musl-release-fix-20260618-6600.log
remote original: /home/yxzhang/oskernel-artifacts/ltp-la-musl-release-fix-20260618-6600.log
```

## Totals

- Total cases walked: 2820
- returned0 cases: 233
- non-returned0 cases classified: 2587

## Exit Code Distribution

| exit_code | count |
| --- | ---: |
| 127 | 1614 |
| 32 | 485 |
| 2 | 173 |
| 1 | 164 |
| 124 | 103 |
| 3 | 13 |
| 36 | 10 |
| 33 | 5 |
| 6 | 5 |
| 34 | 5 |
| 139 | 4 |
| 255 | 2 |
| 128 | 1 |
| 22 | 1 |
| 132 | 1 |
| 35 | 1 |

## Root Cause Clusters

| cluster | count | dominant exit codes | fix cost |
| --- | ---: | --- | --- |
| missing-userland-command | 1373 | 127:1288, 32:76, 2:3, 1:2, 124:2 | low/medium if applet/link packaging; high if full tool required |
| aio-io-filesystem-feature-gap | 245 | 32:133, 1:48, 2:39, 127:8, 124:6 | medium async/fs feature semantics |
| scheduler-time-signal-semantics | 227 | 127:127, 124:56, 32:15, 1:14, 2:10 | medium scheduler/time/signal semantics |
| kernel-config-or-procfs-gap | 200 | 32:88, 1:67, 2:34, 124:5, 139:3 | medium procfs/config emulation |
| memory-vm-semantics | 159 | 127:111, 32:38, 2:3, 1:3, 124:2 | medium VM/memory semantics |
| network-namespace-or-tooling-gap | 108 | 32:47, 127:40, 124:10, 1:5, 36:4 | high network namespace/tool stack |
| testcase-broken-or-tconf | 85 | 32:54, 2:29, 33:1, 127:1 | defer; test/environment unsuitable |
| unsupported-device-or-block-feature | 65 | 2:27, 127:27, 34:5, 32:2, 124:1 | high device/block feature |
| cgroup-controller-gap | 58 | 32:19, 2:11, 124:11, 127:7, 1:6 | high cgroup controller semantics |
| keyring-crypto-security-feature-gap | 37 | 2:16, 32:13, 127:5, 1:2, 33:1 | medium/high keyring/crypto/security feature |
| unknown-nonzero | 19 | 1:16, 128:1, 3:1, 255:1 | needs manual triage |
| runtime-timeout-or-watchdog | 10 | 124:10 | high robustness/performance wall |
| userspace-crash-or-illegal-instruction | 1 | 139:1 | high ABI/runtime crash triage |

## Batch Strategy Notes

- Cheap/high-multiplier candidates are filesystem/path errno semantics, chmod/fchmod/ftruncate-style syscall errno fixes, and simple userland applet/link packaging.
- High-cost clusters are cgroup, network namespace/tooling, device/block, and broad procfs/config emulation.
- Timeout/watchdog and userspace crash clusters are robustness walls and should be handled before relying on deeper full-run coverage.
- The CSV is the authoritative per-case classification; this Markdown is only the aggregate view.

## CSV

- `analysis/ltp-la-musl-nonreturned0-classification-20260618.csv`
