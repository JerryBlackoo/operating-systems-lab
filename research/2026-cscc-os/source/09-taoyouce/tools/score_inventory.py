#!/usr/bin/env python3
"""Enumerate every testcase of autotest-for-oskernel by feeding empty stdin.

Each judge_<name>.py, when given empty stdin, still prints the full list of
items it would grade against (all marked failed / score=0). That list is the
canonical "题库" for that group. We dump counts so the total number of
testcases across all groups is unambiguous.
"""

import json
import os
import subprocess
import sys

JUDGE_DIR = os.path.abspath(
    os.path.join(
        os.path.dirname(os.path.abspath(__file__)),
        "..",
        "reference-works",
        "autotest-for-oskernel",
        "kernel",
        "judge",
    )
)


def judges():
    out = []
    for fn in sorted(os.listdir(JUDGE_DIR)):
        if fn.startswith("judge_") and fn.endswith(".py"):
            out.append(fn[len("judge_") : -len(".py")])
    return out


def inspect(group):
    proc = subprocess.run(
        [sys.executable, os.path.join(JUDGE_DIR, f"judge_{group}.py")],
        input=b"",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=60,
    )
    raw = proc.stdout.decode("utf-8", errors="ignore").strip()
    if not raw:
        return [], 0
    try:
        items = json.loads(raw)
    except json.JSONDecodeError:
        items = json.loads(raw.splitlines()[-1])
    possible = 0
    for it in items:
        if not isinstance(it, dict):
            continue
        possible += it.get("all") or it.get("total") or 1
    return items, possible


def main():
    arches = ("musl", "glibc")
    rows = []
    for g in judges():
        items, possible = inspect(g)
        rows.append((g, len(items), possible))
    total_items = 0
    total_possible = 0
    print(f"{'group':20s}  {'#items':>8s}  {'max score (sum of all/total)':>28s}")
    print("-" * 62)
    for g, n, p in rows:
        print(f"{g:20s}  {n:8d}  {p:28d}")
        total_items += n
        total_possible += p
    print("-" * 62)
    print(f"{'ALL groups x1 arch':20s}  {total_items:8d}  {total_possible:28d}")
    print(f"{'ALL groups x2 archs':20s}  {total_items*2:8d}  {total_possible*2:28d}")

    # Also break down musl vs glibc
    musl = [(g, n, p) for g, n, p in rows if g.endswith("-musl")]
    glibc = [(g, n, p) for g, n, p in rows if g.endswith("-glibc")]
    def sum_items(xs):
        return sum(n for _, n, _ in xs), sum(p for _, _, p in xs)
    mi, mp = sum_items(musl)
    gi, gp = sum_items(glibc)
    print()
    print(f"{'musl-only  x1 arch':20s}  {mi:8d}  {mp:28d}")
    print(f"{'glibc-only x1 arch':20s}  {gi:8d}  {gp:28d}")


if __name__ == "__main__":
    main()
