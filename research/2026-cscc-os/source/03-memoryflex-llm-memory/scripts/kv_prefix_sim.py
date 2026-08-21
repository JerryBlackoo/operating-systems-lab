"""KV 前缀缓存仿真（赛题任务1：KV cache 使用）

在"前缀缓存 + 最近最少使用(LRU)淘汰 + 容量受限"的模型下，对合成请求流
（共享 system prompt、多轮对话、主题复用、agent 突发）做前缀命中仿真，输出：
  - 命中 token 数 / 命中率 随缓存容量变化
  - 单请求命中长度分布

用法: python scripts/kv_prefix_sim.py --out-dir docs/figures
"""

import argparse
import json
import random
from collections import OrderedDict
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt


def gen_trace(n_req, sys_len, n_users, turns_per_user, topic_tokens, seed=42):
    """生成 (user_id, topic, token_len) 请求流；同一用户多轮共享历史，系统提示共享前缀。"""
    rng = random.Random(seed)
    reqs = []
    for u in range(n_users):
        topic = rng.randrange(topic_tokens)
        hist = sys_len
        for t in range(turns_per_user):
            new = rng.randint(8, 64)
            reqs.append({"user": u, "topic": topic, "prefix_len": hist, "new_len": new})
            hist += new
    # 突发：少数高频主题集中出现（agent 场景）
    for _ in range(int(n_req * 0.25)):
        u = rng.randrange(n_users)
        topic = rng.randrange(topic_tokens)
        reqs.append({"user": u, "topic": topic,
                     "prefix_len": sys_len + rng.randint(0, 80), "new_len": rng.randint(8, 64)})
    rng.shuffle(reqs)
    return reqs[:n_req]


class PrefixCache:
    """容量受限的 LRU 前缀缓存：按 token 计数，条目为用户会话前缀。"""

    def __init__(self, capacity_tokens):
        self.cap = capacity_tokens
        self.entries = OrderedDict()   # key -> (tokens_used, prefix_len)
        self.used = 0

    def _evict(self):
        while self.used > self.cap and self.entries:
            _, (n, _) = self.entries.popitem(last=False)
            self.used -= n

    def lookup(self, user, topic, prefix_len, full_len):
        key = (user, topic)
        if key in self.entries:
            hit = min(self.entries[key][1], prefix_len)
            self.entries.move_to_end(key)
        else:
            hit = 0
        # 写回完整前缀（新请求执行后，其前缀进入缓存）
        self.entries[key] = (full_len, full_len)
        self.used += full_len
        self._evict()
        return hit


def run_sweep(reqs, caps):
    out = []
    for cap in caps:
        cache = PrefixCache(cap)
        total_prompt = 0
        total_hit = 0
        hits = []
        for r in reqs:
            full = r["prefix_len"] + r["new_len"]
            hit = cache.lookup(r["user"], r["topic"], r["prefix_len"], full)
            total_prompt += full
            total_hit += hit
            hits.append(hit)
        out.append({"capacity_tokens": cap,
                    "hit_rate": total_hit / total_prompt,
                    "avg_hit_len": total_hit / len(reqs)})
    return out, hits


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out-dir", default="docs/figures")
    ap.add_argument("--n-req", type=int, default=400)
    ap.add_argument("--sys-len", type=int, default=80)
    ap.add_argument("--users", type=int, default=16)
    ap.add_argument("--turns", type=int, default=6)
    args = ap.parse_args()

    reqs = gen_trace(args.n_req, args.sys_len, args.users, args.turns, topic_tokens=8)
    caps = [0, 1024, 2048, 4096, 8192, 16384, 32768, 65536]
    sweep, hits = run_sweep(reqs, caps)
    # 固定容量（16K）下的单请求命中长度分布
    _, hits16k = run_sweep(reqs, [16384])
    hits16k = hits16k[-1] if isinstance(hits16k, list) and hits16k and isinstance(hits16k[0], list) else hits16k

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "kv_prefix_sim.json").write_text(
        json.dumps({"requests": len(reqs), "sweep": sweep, "hits_at_16k": hits16k},
                   ensure_ascii=False, indent=2), encoding="utf-8")

    fig, axes = plt.subplots(1, 2, figsize=(12, 4.2))
    xs = [s["capacity_tokens"] for s in sweep]
    axes[0].plot(xs, [s["hit_rate"] for s in sweep], "o-")
    axes[0].set_xlabel("prefix cache capacity (tokens)"); axes[0].set_ylabel("hit rate")
    axes[0].set_title("prefix hit rate vs cache capacity")
    axes[0].set_xscale("log")
    axes[1].hist(hits, bins=30, edgecolor="black")
    axes[1].set_xlabel("cached prefix tokens per request"); axes[1].set_ylabel("count")
    axes[1].set_title("hit-length distribution (cap=16384)")
    fig.tight_layout()
    fig.savefig(out_dir / "kv_prefix_sim.png", dpi=120)
    plt.close(fig)

    print(f"requests={len(reqs)}")
    for s in sweep:
        print(f"cap={s['capacity_tokens']:>7}  hit_rate={s['hit_rate']:.3f}  avg_hit_len={s['avg_hit_len']:.1f}")
    print(f"saved -> {out_dir}/kv_prefix_sim.json / .png")


if __name__ == "__main__":
    main()
