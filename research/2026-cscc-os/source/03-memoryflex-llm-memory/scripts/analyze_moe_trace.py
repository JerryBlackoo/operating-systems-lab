"""专家激活轨迹解析（赛题任务1：专家激活）

解析 llama.cpp LLAMA_MOE_TRACE 输出的二进制记录（router probs 版本）：
  int32 layer, int32 n_expert, int32 n_tokens, n_expert*n_tokens float32 probs
对每个 token 每层按概率取 top-k 作为激活专家。
统计每个生成 token 每层激活的专家集合，输出：
  - 每层/整体 专家激活频次（热点专家）
  - 每层被激活的专家数与覆盖率（64 专家取 8 的稀疏性）
  - 激活专家集合的时间稳定性（相邻 token 的重叠率）

用法: python scripts/analyze_moe_trace.py --trace path.bin --out-dir docs/figures
"""

import argparse
import json
import struct
from collections import Counter
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt


def parse_trace(path, n_used):
    records = []
    with open(path, "rb") as f:
        while True:
            hdr = f.read(12)
            if len(hdr) < 12:
                break
            layer, n_exp, n_tok = struct.unpack("<iii", hdr)
            data = f.read(4 * n_exp * n_tok)
            if len(data) < 4 * n_exp * n_tok:
                break
            probs = list(struct.unpack(f"<{n_exp * n_tok}f", data))
            # 每个 token 取 top-n_used 专家索引（按概率降序）
            ids = []
            for t in range(n_tok):
                ps = probs[t * n_exp:(t + 1) * n_exp]
                ids.append(sorted(range(n_exp), key=lambda e: ps[e], reverse=True)[:n_used])
            records.append({"layer": layer, "n_exp": n_exp, "n_tok": n_tok, "ids": ids})
    return records


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trace", required=True)
    ap.add_argument("--out-dir", default="docs/figures")
    ap.add_argument("--n-expert", type=int, default=64)
    ap.add_argument("--n-used", type=int, default=8)
    args = ap.parse_args()

    records = parse_trace(args.trace, args.n_used)
    # 只保留 decode 步（单 token）
    dec = [r for r in records if r["n_tok"] == 1]
    pre = [r for r in records if r["n_tok"] > 1]
    n_layers = max(r["layer"] for r in dec) + 1 if dec else 0
    n_tokens = len(dec) // max(1, n_layers)

    layer_freq = [Counter() for _ in range(n_layers)]
    layer_used = []
    overlap = []
    prev = None
    for t in range(n_tokens):
        cur = []
        for l in range(n_layers):
            r = dec[t * n_layers + l]
            ids = r["ids"][0]
            layer_freq[l].update(ids)
            cur.append(set(ids))
        layer_used.append([len(s) for s in cur])
        if prev is not None:
            jac = [len(a & b) / max(1, len(a | b)) for a, b in zip(prev, cur)]
            overlap.append(sum(jac) / len(jac))
        prev = cur

    total_activated = set()
    for lf in layer_freq:
        total_activated.update(lf.keys())
    overall = Counter()
    for lf in layer_freq:
        overall.update(lf)

    result = {
        "n_layers": n_layers, "n_expert": args.n_expert,
        "n_decode_tokens": n_tokens,
        "n_prefill_records": len(pre),
        "n_expert_used_per_token": args.n_used,
        "layer_activated_experts": [len(lf) for lf in layer_freq],
        "layer_coverage_pct": [round(100 * len(lf) / args.n_expert, 1) for lf in layer_freq],
        "layer_top_experts": {str(i): lf.most_common(20) for i, lf in enumerate(layer_freq)},
        "activated_any_layer": len(total_activated),
        "activated_any_layer_pct": round(100 * len(total_activated) / args.n_expert, 1),
        "top10_experts_overall": overall.most_common(10),
        "expert_freq": [overall.get(e, 0) for e in range(args.n_expert)],
        "concentration": _concentration(overall, args.n_expert),
        "activation_mass_top10_pct": round(100 * sum(c for _, c in overall.most_common(10)) /
                                           max(1, sum(overall.values())), 1),
        "adjacent_token_overlap_mean": round(sum(overlap) / len(overlap), 3) if overlap else None,
        "per_token_activated": layer_used,
    }

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "moe_trace_stats.json").write_text(
        json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")

    fig, axes = plt.subplots(1, 3, figsize=(15, 4.2))
    im = axes[0].imshow([[layer_freq[l].get(e, 0) for e in range(args.n_expert)]
                         for l in range(n_layers)], aspect="auto", cmap="magma")
    axes[0].set_xlabel("expert id"); axes[0].set_ylabel("layer")
    axes[0].set_title("expert activation frequency (per layer)")
    fig.colorbar(im, ax=axes[0])
    axes[1].bar(range(n_layers), result["layer_coverage_pct"])
    axes[1].set_xlabel("layer"); axes[1].set_ylabel("% experts ever activated")
    axes[1].set_title(f"layer coverage of {args.n_expert} experts")
    axes[1].set_ylim(0, 105)
    axes[2].hist([v for row in layer_used for v in row], bins=range(0, args.n_expert + 2), edgecolor="black")
    axes[2].set_xlabel("activated experts per token per layer")
    axes[2].set_ylabel("count")
    axes[2].set_title("activation sparsity per token")
    fig.tight_layout()
    # 学术图表由 scripts/figures_phase1.py 的 fig7 生成，此处仅输出统计 JSON
    plt.close(fig)

    print(f"layers={n_layers} decode_tokens={n_tokens} expert={args.n_expert}")
    print(f"activated in any layer: {len(total_activated)}/{args.n_expert} "
          f"({result['activated_any_layer_pct']}%)")
    print(f"layer coverage %: {result['layer_coverage_pct']}")
    print(f"top10 overall: {overall.most_common(10)}")
    print(f"adjacent-token overlap: {result['adjacent_token_overlap_mean']}")
    print(f"saved -> {out_dir}/moe_trace_stats.json / moe_activation.png")


def _concentration(overall, n_expert):
    """按专家激活频次降序的累积占比序列（Lorenz 曲线）。"""
    total = max(1, sum(overall.values()))
    cum = 0.0
    out = []
    for _, c in overall.most_common(n_expert):
        cum += c
        out.append(round(cum / total, 4))
    return out


if __name__ == "__main__":
    main()
