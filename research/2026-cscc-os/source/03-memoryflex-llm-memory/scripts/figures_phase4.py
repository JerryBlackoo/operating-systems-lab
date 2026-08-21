"""阶段4学术风格图件生成（LMETRIC 乘法调度）

读取 results/cluster_*.json，输出：
  fig16_policy_compare.png   三策略多指标对比（TTFT/命中率/负载均衡）
  fig17_kv_hit_mechanism.png KV 命中机制：命中 vs 冷启动 prefill
  fig18_hotspot_detector.png 热点检测器：无/有检测器的实例负载分布

用法: python scripts/figures_phase4.py
"""

import json
import statistics
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np


RES = Path(__file__).resolve().parent.parent / "results"
FIG = Path(__file__).resolve().parent.parent / "docs/figures"

PALETTE = ["#4C72B0", "#DD8452", "#55A868", "#C44E52", "#8172B3"]

plt.rcParams.update({
    "font.family": "serif",
    "font.serif": ["Times New Roman", "DejaVu Serif"],
    "font.size": 9.5,
    "axes.titlesize": 10.5,
    "axes.labelsize": 10,
    "xtick.labelsize": 8.5,
    "ytick.labelsize": 8.5,
    "legend.fontsize": 8.2,
    "axes.spines.top": False,
    "axes.spines.right": False,
    "axes.grid": True,
    "grid.alpha": 0.25,
    "grid.linewidth": 0.6,
    "figure.dpi": 200,
    "savefig.bbox": "tight",
})


def load(name):
    return json.loads((RES / name).read_text(encoding="utf-8"))


def fig16_policy_compare():
    pols = ["lb", "lmetric", "linear"]
    labels = ["LB-only\n(JSQ)", "LMETRIC\n(P×BS)", "Linear\n(tuned)"]
    s = {p: load(f"cluster_{p}.json") for p in pols}
    fig, axes = plt.subplots(1, 3, figsize=(10.5, 3.1))
    ttft = [s[p]["ttft_mean_ms"] for p in pols]
    hit = [s[p]["kv_hit_rate"] for p in pols]
    std = [s[p]["load_balance_std"] for p in pols]
    colors = ["0.62", PALETTE[0], PALETTE[3]]
    x = np.arange(3)
    axes[0].bar(x, ttft, color=colors, width=0.55)
    for i, v in enumerate(ttft):
        axes[0].text(i, v + 2, f"{v:.0f}", ha="center", fontsize=8.2)
    axes[0].set_xticks(x); axes[0].set_xticklabels(labels, fontsize=8)
    axes[0].set_ylabel("mean TTFT (ms)")
    axes[0].set_title("(a) time-to-first-token", fontsize=9.5)
    axes[0].annotate("−6.2% vs LB\n(zero tuning)", xy=(1, ttft[1]),
                     xytext=(1.28, ttft[1] + 18), fontsize=8, color=PALETTE[0],
                     arrowprops=dict(arrowstyle="->", lw=0.8))
    axes[1].bar(x, [h * 100 for h in hit], color=colors, width=0.55)
    for i, v in enumerate([h * 100 for h in hit]):
        axes[1].text(i, v + 1.5, f"{v:.0f}%", ha="center", fontsize=8.2)
    axes[1].set_xticks(x); axes[1].set_xticklabels(labels, fontsize=8)
    axes[1].set_ylabel("KV prefix hit rate")
    axes[1].set_ylim(0, 110)
    axes[1].set_title("(b) KV cache hits", fontsize=9.5)
    axes[2].bar(x, std, color=colors, width=0.55)
    for i, v in enumerate(std):
        axes[2].text(i, v + 0.4, f"{v:.0f}", ha="center", fontsize=8.2)
    axes[2].set_xticks(x); axes[2].set_xticklabels(labels, fontsize=8)
    axes[2].set_ylabel("load imbalance σ (requests/instance)")
    axes[2].set_title("(c) load balance", fontsize=9.5)
    axes[2].annotate("LMETRIC = LB balance\n+ KV-aware hits", xy=(1, 8),
                     xytext=(0.45, 17), fontsize=8, color=PALETTE[0])
    fig.tight_layout()
    fig.savefig(FIG / "fig16_policy_compare.png")
    plt.close(fig)


def fig17_kv_hit_mechanism():
    d = load("cluster_lmetric.detail.json")
    ok = [r for r in d if r.get("prefill_ms")]
    hi = [r["prefill_ms"] for r in ok if r["hit_len"] // 4 > 40]
    lo = [r["prefill_ms"] for r in ok if r["hit_len"] // 4 <= 40]
    fig, axes = plt.subplots(1, 2, figsize=(9.0, 3.0))
    axes[0].bar(["cold (hit ≤ 40 tok)", "KV hit (hit > 40 tok)"],
                [statistics.mean(lo), statistics.mean(hi)],
                color=[PALETTE[3], PALETTE[2]], width=0.5)
    for i, v in enumerate([statistics.mean(lo), statistics.mean(hi)]):
        axes[0].text(i, v + 3, f"{v:.0f} ms", ha="center", fontsize=8.4)
    axes[0].set_ylabel("prefill time (ms)")
    axes[0].set_title("(a) KV hit reduces prefill")
    axes[0].annotate(f"Δ = −{100*(1-statistics.mean(hi)/statistics.mean(lo)):.0f}%",
                     xy=(0.5, 0.92), xycoords="axes fraction", ha="center",
                     fontsize=9, color=PALETTE[2], fontweight="bold")
    # 命中长度分布（LMETRIC）
    hits = sorted(r["hit_len"] // 4 for r in ok)
    axes[1].hist(hits, bins=16, color=PALETTE[0], edgecolor="white")
    axes[1].axvline(40, color=PALETTE[3], ls="--", lw=1.0)
    axes[1].text(42, axes[1].get_ylim()[1] * 0.9, "cold threshold", fontsize=7.6, color=PALETTE[3])
    axes[1].set_xlabel("cached prefix tokens per request")
    axes[1].set_ylabel("request count")
    axes[1].set_title("(b) hit-length distribution (LMETRIC)")
    fig.tight_layout()
    fig.savefig(FIG / "fig17_kv_hit_mechanism.png")
    plt.close(fig)


def fig18_hotspot_detector():
    a = load("cluster_hotspot_lm.json")
    b = load("cluster_hotspot_lm_det.json")
    da = load("cluster_hotspot_lm.detail.json")
    db = load("cluster_hotspot_lm_det.detail.json")
    ca = [sum(1 for r in da if r["instance"] == i) for i in range(4)]
    cb = [sum(1 for r in db if r["instance"] == i) for i in range(4)]
    fig, axes = plt.subplots(1, 2, figsize=(9.0, 3.0), sharey=True)
    x = np.arange(4)
    axes[0].bar(x, ca, color=PALETTE[3], width=0.6)
    axes[0].set_xticks(x); axes[0].set_xticklabels([f"E{i}" for i in range(4)])
    axes[0].set_ylabel("requests served")
    axes[0].set_title("(a) LMETRIC without detector")
    axes[0].annotate("hotspot: 33/42 on E1", xy=(1, ca[1]), xytext=(1.2, ca[1] - 8),
                     fontsize=8, color=PALETTE[3], arrowprops=dict(arrowstyle="->", lw=0.8))
    axes[1].bar(x, cb, color=PALETTE[0], width=0.6)
    axes[1].set_xticks(x); axes[1].set_xticklabels([f"E{i}" for i in range(4)])
    axes[1].set_title("(b) LMETRIC + hotspot detector")
    axes[1].annotate(f"{b.get('n_alarms', 0)} alarms → LB fallback",
                     xy=(0.5, 0.94), xycoords="axes fraction", ha="center",
                     fontsize=8.4, color=PALETTE[0])
    fig.tight_layout()
    fig.savefig(FIG / "fig18_hotspot_detector.png")
    plt.close(fig)


def main():
    fig16_policy_compare()
    fig17_kv_hit_mechanism()
    fig18_hotspot_detector()
    print("phase4 figures written to", FIG)


if __name__ == "__main__":
    main()
