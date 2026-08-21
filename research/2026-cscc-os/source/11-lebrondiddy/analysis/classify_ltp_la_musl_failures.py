#!/usr/bin/env python3
"""Classify non-returned0 LTP cases from the accepted la-musl full log."""

from __future__ import annotations

import csv
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path


SIGNALS = [
    ("missing-userland-command", r"(not found|No such file or directory|can't execute|command .* not found)"),
    ("dynamic-loader-or-library-gap", r"(error while loading shared libraries|ld-musl|lib.*\.so|ELF interpreter)"),
    ("kernel-config-or-procfs-gap", r"(Constraint 'CONFIG_|/proc/config|procfs|/proc/|does not exist|Undefined)"),
    ("filesystem-or-path-permission-semantics", r"(EACCES|EPERM|ENOENT|ENOTDIR|EROFS|ELOOP|permission|Operation not permitted|Failed to statfs|open\()"),
    ("network-namespace-or-tooling-gap", r"(IPv6 disabled|ip' not found|iptables|macsec|netns|NET_NS|USER_NS|VETH|socket|bind|connect|icmp|dhcp)"),
    ("cgroup-controller-gap", r"(cgroup|memcg|cpuset|cpuacct|freezer|cpuctl)"),
    ("keyring-crypto-security-feature-gap", r"(keyctl|add_key|request_key|AF_ALG|crypto|ima_|SELinux|apparmor)"),
    ("aio-io-filesystem-feature-gap", r"(aio|io_uring|fallocate|fcntl|flock|inotify|fanotify|fs_|dio|sync_file_range|xattr|acl)"),
    ("scheduler-time-signal-semantics", r"(clock_|timer|nanosleep|alarm|signal|kill|sched|futex|wait|clone|fork|ptrace|getrusage|getpriority)"),
    ("memory-vm-semantics", r"(mmap|mprotect|madvise|mlock|mincore|brk|oom|memcpy|memcmp|hugemmap|hugetlb|shm|malloc)"),
    ("unsupported-device-or-block-feature", r"(zram|loop|eject|block|blk|mount|umount|ioctl_ns|device|scsi|hdparm)"),
    ("testcase-broken-or-tconf", r"(TCONF|TBROK|TWARN|Remaining cases broken|Aborting due to unsuitable)"),
    ("runtime-timeout-or-watchdog", r"(wall-clock limit|TimedOut|timeout|exceeded|SIGKILL semantics)"),
    ("userspace-crash-or-illegal-instruction", r"(Segmentation fault|Illegal instruction|signal 11|signal 4|SIGSEGV|SIGILL)"),
]

PREFIX_HINTS = [
    ("cgroup-controller-gap", ("cgroup", "memcg", "cpuset", "cpuacct", "cpuctl", "freezer")),
    ("network-namespace-or-tooling-gap", ("icmp", "if-", "if4-", "if6-", "dhcp", "macsec", "iptables", "ftp-", "dns-", "nfs", "rpc")),
    ("keyring-crypto-security-feature-gap", ("add_key", "keyctl", "request_key", "af_alg", "ima_")),
    ("aio-io-filesystem-feature-gap", ("aio", "aiodio", "fcntl", "flock", "fanotify", "inotify", "fallocate", "fs_", "dio", "acl")),
    ("scheduler-time-signal-semantics", ("clock_", "timer", "alarm", "kill", "futex", "clone", "fork", "sched", "wait")),
    ("memory-vm-semantics", ("mmap", "mprotect", "madvise", "mlock", "mincore", "mem", "hugemmap", "hugetlb")),
    ("unsupported-device-or-block-feature", ("zram", "eject", "ioctl_ns", "loop", "block")),
]

COST_BY_CLUSTER = {
    "missing-userland-command": "low/medium if applet/link packaging; high if full tool required",
    "dynamic-loader-or-library-gap": "medium packaging/library staging",
    "kernel-config-or-procfs-gap": "medium procfs/config emulation",
    "filesystem-or-path-permission-semantics": "low/medium syscall errno and VFS semantics",
    "network-namespace-or-tooling-gap": "high network namespace/tool stack",
    "cgroup-controller-gap": "high cgroup controller semantics",
    "keyring-crypto-security-feature-gap": "medium/high keyring/crypto/security feature",
    "aio-io-filesystem-feature-gap": "medium async/fs feature semantics",
    "scheduler-time-signal-semantics": "medium scheduler/time/signal semantics",
    "memory-vm-semantics": "medium VM/memory semantics",
    "unsupported-device-or-block-feature": "high device/block feature",
    "testcase-broken-or-tconf": "defer; test/environment unsuitable",
    "runtime-timeout-or-watchdog": "high robustness/performance wall",
    "userspace-crash-or-illegal-instruction": "high ABI/runtime crash triage",
    "unknown-nonzero": "needs manual triage",
}


def split_records(text: str):
    run_re = re.compile(r"^RUN LTP CASE (.*)$", re.M)
    runs = [(m.group(1), m.start()) for m in run_re.finditer(text)]
    for index, (name, start) in enumerate(runs):
        end = runs[index + 1][1] if index + 1 < len(runs) else len(text)
        block = text[start:end]
        fail = re.search(r"^FAIL LTP CASE " + re.escape(name) + r" : (\d+)$", block, re.M)
        code = int(fail.group(1)) if fail else None
        yield index + 1, name, code, block


def classify(name: str, code: int | None, block: str):
    lower_name = name.lower()
    for cluster, prefixes in PREFIX_HINTS:
        if lower_name.startswith(prefixes):
            return cluster

    for cluster, pattern in SIGNALS:
        if re.search(pattern, block, re.I):
            return cluster

    if code == 124:
        return "runtime-timeout-or-watchdog"
    if code == 127:
        return "missing-userland-command"
    if code in (139, 132):
        return "userspace-crash-or-illegal-instruction"
    if code in (32, 36):
        return "testcase-broken-or-tconf"
    return "unknown-nonzero"


def evidence(block: str) -> str:
    interesting = []
    for line in block.splitlines()[1:]:
        stripped = line.strip()
        if not stripped:
            continue
        if re.search(
            r"(TCONF|TBROK|TFAIL|TWARN|not found|No such file|can't execute|error while loading|"
            r"Constraint|Undefined|Failed|errno=|wall-clock|Segmentation|Illegal|FAIL LTP CASE)",
            stripped,
            re.I,
        ):
            interesting.append(stripped)
        if len(interesting) >= 3:
            break
    if not interesting:
        for line in block.splitlines()[1:4]:
            stripped = line.strip()
            if stripped:
                interesting.append(stripped)
    return " | ".join(interesting)[:500]


def main() -> int:
    if len(sys.argv) != 4:
        print(
            "usage: classify_ltp_la_musl_failures.py <log> <csv-out> <md-out>",
            file=sys.stderr,
        )
        return 2

    log_path = Path(sys.argv[1])
    csv_path = Path(sys.argv[2])
    md_path = Path(sys.argv[3])
    text = log_path.read_text(errors="replace").replace("\r", "")

    rows = []
    total = returned0 = 0
    for index, name, code, block in split_records(text):
        total += 1
        if code == 0:
            returned0 += 1
            continue
        cluster = classify(name, code, block)
        rows.append(
            {
                "index": index,
                "case": name,
                "exit_code": "" if code is None else str(code),
                "root_cause_cluster": cluster,
                "fix_cost": COST_BY_CLUSTER[cluster],
                "evidence": evidence(block),
            }
        )

    csv_path.parent.mkdir(parents=True, exist_ok=True)
    with csv_path.open("w", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "index",
                "case",
                "exit_code",
                "root_cause_cluster",
                "fix_cost",
                "evidence",
            ],
        )
        writer.writeheader()
        writer.writerows(rows)

    cluster_counts = Counter(row["root_cause_cluster"] for row in rows)
    code_counts = Counter(row["exit_code"] for row in rows)
    cluster_code_counts: dict[str, Counter[str]] = defaultdict(Counter)
    for row in rows:
        cluster_code_counts[row["root_cause_cluster"]][row["exit_code"]] += 1

    md = [
        "# 2026-06-18 la-musl non-returned0 classification",
        "",
        "Source log:",
        "",
        "```text",
        str(log_path),
        "remote original: /home/yxzhang/oskernel-artifacts/ltp-la-musl-release-fix-20260618-6600.log",
        "```",
        "",
        "## Totals",
        "",
        f"- Total cases walked: {total}",
        f"- returned0 cases: {returned0}",
        f"- non-returned0 cases classified: {len(rows)}",
        "",
        "## Exit Code Distribution",
        "",
        "| exit_code | count |",
        "| --- | ---: |",
    ]
    for code, count in code_counts.most_common():
        md.append(f"| {code} | {count} |")

    md.extend(
        [
            "",
            "## Root Cause Clusters",
            "",
            "| cluster | count | dominant exit codes | fix cost |",
            "| --- | ---: | --- | --- |",
        ]
    )
    for cluster, count in cluster_counts.most_common():
        codes = ", ".join(f"{code}:{n}" for code, n in cluster_code_counts[cluster].most_common(5))
        md.append(f"| {cluster} | {count} | {codes} | {COST_BY_CLUSTER[cluster]} |")

    md.extend(
        [
            "",
            "## Batch Strategy Notes",
            "",
            "- Cheap/high-multiplier candidates are filesystem/path errno semantics, chmod/fchmod/ftruncate-style syscall errno fixes, and simple userland applet/link packaging.",
            "- High-cost clusters are cgroup, network namespace/tooling, device/block, and broad procfs/config emulation.",
            "- Timeout/watchdog and userspace crash clusters are robustness walls and should be handled before relying on deeper full-run coverage.",
            "- The CSV is the authoritative per-case classification; this Markdown is only the aggregate view.",
            "",
            "## CSV",
            "",
            f"- `analysis/{csv_path.name}`",
            "",
        ]
    )
    md_path.write_text("\n".join(md))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
