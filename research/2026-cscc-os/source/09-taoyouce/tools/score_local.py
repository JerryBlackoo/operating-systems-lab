#!/usr/bin/env python3
"""Offline rerun of the official autotest-for-oskernel judges against a serial log.

Mirrors reference-works/autotest-for-oskernel/kernel/run.py:parse_serial_out_new
exactly: for each "#### OS COMP TEST GROUP START <name> ####" ... "#### OS COMP
TEST GROUP END ..." block, pipe the block body into judge_<name>.py through
stdin and read JSON from stdout. Groups never seen in the log are still invoked
with empty stdin so missing-group penalties apply identically to the server.
"""

import json
import os
import re
import subprocess
import sys
from collections import OrderedDict

JUDGE_DIR = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "reference-works",
    "autotest-for-oskernel",
    "kernel",
    "judge",
)
JUDGE_DIR = os.path.abspath(JUDGE_DIR)

START_RE = re.compile(r"#### OS COMP TEST GROUP START ([a-zA-Z0-9-]+) ####")
END_MARK = "#### OS COMP TEST GROUP END"


def _judge_cmd(name: str) -> list:
    path = os.path.join(JUDGE_DIR, f"judge_{name}.py")
    return [sys.executable, path]


def _known_groups() -> list:
    out = []
    for fn in os.listdir(JUDGE_DIR):
        if fn.startswith("judge_") and fn.endswith(".py"):
            out.append(fn[len("judge_") : -len(".py")])
    return sorted(out)


def _run_judge(name: str, payload: str) -> list:
    proc = subprocess.run(
        _judge_cmd(name),
        input=payload.encode("utf-8", errors="ignore"),
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
        # Some judges print status-then-json; keep last JSON-looking line
        last = raw.splitlines()[-1]
        return json.loads(last)


def score_serial(path: str) -> OrderedDict:
    ans: OrderedDict = OrderedDict()
    current = None
    buffer: list = []
    known = set(_known_groups())
    seen = set()
    with open(path, "r", encoding="utf-8", errors="ignore") as f:
        for line in f:
            m = START_RE.search(line)
            if m:
                if current is not None and current in known:
                    ans[current] = _run_judge(current, "".join(buffer))
                current = m.group(1)
                buffer = []
                seen.add(current)
                continue
            if END_MARK in line:
                if current is not None and current in known:
                    ans[current] = _run_judge(current, "".join(buffer))
                current = None
                buffer = []
                continue
            if current is not None:
                buffer.append(line)
    if current is not None and current in known:
        ans[current] = _run_judge(current, "".join(buffer))
    for g in sorted(known - seen):
        ans[g] = _run_judge(g, "")
    return ans


def summarize(tag: str, ans: dict) -> int:
    total = 0
    print(f"\n===== {tag} =====")
    for group, items in ans.items():
        if not isinstance(items, list):
            print(f"  [skip] {group}: non-list result -> {items!r}")
            continue
        gained = 0
        possible = 0
        passed_names = []
        failed_names = []
        for it in items:
            if not isinstance(it, dict):
                continue
            name = it.get("name", "?")
            score = it.get("score", it.get("pass", 0))
            try:
                score = int(score) if float(score).is_integer() else float(score)
            except (TypeError, ValueError):
                score = 0
            p = it.get("pass")
            all_cnt = it.get("all") or it.get("total") or 1
            possible += all_cnt if isinstance(all_cnt, (int, float)) else 1
            gained += score
            ok = (p == 1) or (isinstance(score, (int, float)) and float(score) >= 1.0)
            (passed_names if ok else failed_names).append(name)
        total += gained
        print(
            f"  {group:18s}  pass={len(passed_names):3d}/{len(items):3d}"
            f"  gained={gained}  possible={possible}"
        )
        if failed_names:
            preview = ", ".join(failed_names[:6])
            more = f" ... (+{len(failed_names)-6} more)" if len(failed_names) > 6 else ""
            print(f"      failed: {preview}{more}")
    print(f"  TOTAL items scored >= 1: {total}")
    return total


def main() -> None:
    if len(sys.argv) < 2:
        print("usage: score_local.py <serial_log> [<serial_log>...]", file=sys.stderr)
        sys.exit(2)
    grand = 0
    for path in sys.argv[1:]:
        ans = score_serial(path)
        grand += summarize(path, ans)
    print(f"\n=== GRAND TOTAL items scored >= 1: {grand}")


if __name__ == "__main__":
    main()
