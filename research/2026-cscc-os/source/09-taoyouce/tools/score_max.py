#!/usr/bin/env python3
"""Compute the theoretical maximum score of autotest-for-oskernel.

Uses the same aggregation as `reference-works/autotest-for-oskernel/kernel/postwork.py::postwork`:
    score = sum over (arch in ['rv','la']) over (group in all judges) over (item) of item['score']

For each judge we derive the per-item upper bound directly from the judge source:
    - basic-*      : sum of `count` per TestBase subclass (pass-all-asserts)      -> 102 each group
    - busybox-*    : number of baseline cmd lines                                  -> 53 each group
    - lua-*        : 9 fixed testcases                                               -> 9 each group
    - libctest-*   : len(parse_libctest(libctest_baseline))                          -> 220 each group
    - libcbench-*  : each of N items can reach at most 2.0 (2 - 1/score when score>>1)
    - lmbench-*    : same 2.0 cap per metric
    - iozone-*     : same 2.0 cap
    - iperf-*      : same 2.0 cap
    - netperf-*    : same 2.0 cap
    - cyclictest-* : same 2.0 cap
    - ltp-*        : new-version postwork does NOT include ltp (only old postwork did, capped 1000)
"""

import json
import os
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
JUDGE_DIR = os.path.join(ROOT, "reference-works", "autotest-for-oskernel", "kernel", "judge")


def empty_items(group):
    proc = subprocess.run(
        [sys.executable, os.path.join(JUDGE_DIR, f"judge_{group}.py")],
        input=b"",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=60,
    )
    raw = proc.stdout.decode("utf-8", errors="ignore").strip()
    if not raw:
        return []
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return json.loads(raw.splitlines()[-1])


PERFORMANCE = {"libcbench", "lmbench", "iozone", "iperf", "netperf", "cyclictest"}
CORRECTNESS = {"basic", "busybox", "lua", "libctest"}

# LTP judge returns score = passed-asserts count per case, and the item list
# depends on stdin (empty -> 0 items). We estimate the per-column cap from:
#   - directory size of ltp/testcases/bin on sdcard (~1.4 GB, thousands of
#     binaries; see RISC-V serial log `du` output 1447926 KB);
#   - judge_ltp's sample template, 1~6 passed asserts per case, mean ~2.5;
#   - observed leaderboard #1 of 2026 preliminaries: single ltp column = 4145.
# Using a slightly larger empirical ceiling of 5000 to allow headroom.
LTP_EMPIRICAL_CAP_PER_COLUMN = 5000

# Baseline of current leaderboard top-1 ltp single column (for progress comparison)
LTP_LEADER_SINGLE_COLUMN = 4145


def per_item_max(group_family, item):
    if group_family in PERFORMANCE:
        return 2.0
    if group_family == "basic":
        return item.get("all", item.get("total", 1))
    return 1  # busybox / lua / libctest


def max_for_group(group):
    family = group.rsplit("-", 1)[0]
    if family == "ltp":
        return float(LTP_EMPIRICAL_CAP_PER_COLUMN), LTP_EMPIRICAL_CAP_PER_COLUMN
    items = empty_items(group)
    if not items:
        return 0.0, 0
    per_arch_max = sum(per_item_max(family, it) for it in items)
    return per_arch_max, len(items)


def main():
    groups = sorted(
        fn[len("judge_") : -len(".py")]
        for fn in os.listdir(JUDGE_DIR)
        if fn.startswith("judge_") and fn.endswith(".py")
    )

    print(f"{'group':20s}  {'items':>6s}  {'max/arch':>10s}  {'max×2 arch':>12s}")
    print("-" * 58)
    total_per_arch = 0.0
    total_items_per_arch = 0
    for g in groups:
        m, n = max_for_group(g)
        print(f"{g:20s}  {n:6d}  {m:10.2f}  {m*2:12.2f}")
        total_per_arch += m
        total_items_per_arch += n
    print("-" * 58)
    print(f"{'TOTAL (one arch)':20s}  {total_items_per_arch:6d}  {total_per_arch:10.2f}  {total_per_arch*2:12.2f}")

    # Breakdown by libc
    print()
    for lib in ("musl", "glibc"):
        s = 0.0
        c = 0
        for g in groups:
            if g.endswith(f"-{lib}"):
                m, n = max_for_group(g)
                s += m
                c += n
        print(f"  {lib:>5s} family per arch  items={c}  max={s:.2f}  two-arch max={s*2:.2f}")

    print()
    print(f"Note: ltp-* caps are empirical estimates (cap={LTP_EMPIRICAL_CAP_PER_COLUMN}")
    print(f"per arch/libc column; leaderboard top-1 observed = {LTP_LEADER_SINGLE_COLUMN}).")
    print("Actual LTP upper bound depends on sdcard's ltp/testcases/bin inventory")
    print("and each case's `passed` count; tune LTP_EMPIRICAL_CAP_PER_COLUMN if needed.")


if __name__ == "__main__":
    main()
