"""合成 KV 热点场景（阶段4：两阶段热点检测器验证）

构造一个"热点前缀突发"：单个用户连续 30 轮共享前缀请求（热点），
混入少量其他用户背景请求。无检测器时 LMETRIC 会把突发全部路由到
持有该前缀的少数实例，造成负载失衡；检测器应告警并回退负载均衡。

用法: python scripts/gen_hotspot_trace.py --out results/trace_hotspot.jsonl
"""

import argparse
import json
import random
from pathlib import Path

from gen_chat_trace import system_for, FOLLOW_UPS


def gen(burst=30, bg=12, seed=7):
    rng = random.Random(seed)
    reqs = []
    history = []
    sysp = system_for("hot0")
    for t in range(burst):
        q = FOLLOW_UPS[(t) % len(FOLLOW_UPS)]
        hist = "\n".join(history)
        reqs.append({"user": "hot0", "topic": "hot", "turn": t,
                     "prompt": f"{sysp}\n用户历史：{hist}\n新问题：{q}"})
        history.append(f"问：{q} 答：好的，我来回答。")
    # 背景请求（其他用户，普通多轮）
    for b in range(bg):
        u = f"bg{b % 3}"
        syspb = system_for(u)
        q = FOLLOW_UPS[(b + 3) % len(FOLLOW_UPS)]
        reqs.append({"user": u, "topic": "bg", "turn": 0,
                     "prompt": f"{syspb}\n新问题：{q}"})
    # 连续突发：热点请求连续到达（最坏情况），背景请求随后
    return reqs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="results/trace_hotspot.jsonl")
    ap.add_argument("--burst", type=int, default=30)
    args = ap.parse_args()
    reqs = gen(args.burst)
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as f:
        for r in reqs:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"wrote {len(reqs)} requests (burst={args.burst}) -> {args.out}")


if __name__ == "__main__":
    main()
