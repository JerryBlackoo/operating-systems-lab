"""阶段3学术风格图件生成（异步预取与 IO 隐藏）

读取 results/membudget_*_k*.json，输出：
  fig13_prefetch_window.png   预取窗口扫描（dense：吞吐/TPOT vs k）
  fig14_io_overlap.png        IO 隐藏比例 vs 实测增益（重叠度分析）
  fig15_moe_prefetch.png      MoE 专家预取策略对比（统计/动态 vs 基线）

用法: python scripts/figures_phase3.py
"""

import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np


RES = Path(__file__).resolve().parent.parent / "results"
FIG = Path(__file__).resolve().parent.parent / "docs/figures"

PALETTE = ["#4C72B0", "#DD8452", "#55A868", "#C44E52", "#8172B3", "#937860"]

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


def first(d):
    return d["sweep"][0]


def fig13_prefetch_window():
    d15 = {k: first(load(f"membudget_1.5b_k{k}.json")) for k in (0, 4, 8, 12, 16)}
    d3 = {k: first(load(f"membudget_3b_k{k}.json")) for k in (0, 12, 24)}
    fig, axes = plt.subplots(1, 2, figsize=(9.2, 3.1))
    for ax, dd, label, c in [
        (axes[0], d15, "Qwen2.5-1.5B", PALETTE[0]),
        (axes[1], d3, "Qwen2.5-3B", PALETTE[1])]:
        ks = sorted(dd)
        tps = [dd[k]["decode_tps"] for k in ks]
        tpot = [dd[k]["tpot_mean_ms"] for k in ks]
        ax.plot(ks, tps, "o-", color=c, lw=1.7, label="throughput")
        ax.set_xlabel("prefetch window $k$ (layers)")
        ax.set_ylabel("decode throughput (tok/s)")
        ax.set_title(label, fontsize=10)
        best = max(range(len(ks)), key=lambda i: tps[i])
        gain = 100 * (tps[best] - tps[0]) / tps[0]
        ax.annotate(f"k={ks[best]}: +{gain:.0f}%", xy=(ks[best], tps[best]),
                    xytext=(ks[best] + 0.6, tps[best] - 0.35), fontsize=8.5,
                    color=c, arrowprops=dict(arrowstyle="->", lw=0.8))
        ax2 = ax.twinx()
        ax2.plot(ks, tpot, "s--", color=PALETTE[3], lw=1.3, label="TPOT")
        ax2.set_ylabel("TPOT (ms)")
        ax2.grid(False)
    axes[0].legend(frameon=True, fontsize=7.4, loc="upper right")
    axes[1].legend(frameon=True, fontsize=7.4, loc="upper right")
    fig.savefig(FIG / "fig13_prefetch_window.png")
    plt.close(fig)


def fig14_io_overlap():
    """IO 隐藏比例（窗口字节/未锁定字节）与实测吞吐增益"""
    d15 = {k: first(load(f"membudget_1.5b_k{k}.json")) for k in (0, 4, 8, 12, 16)}
    d3 = {k: first(load(f"membudget_3b_k{k}.json")) for k in (0, 12, 24)}
    fig, ax = plt.subplots(figsize=(5.8, 3.2))
    for dd, label, c in [(d15, "Qwen2.5-1.5B", PALETTE[0]),
                         (d3, "Qwen2.5-3B", PALETTE[1])]:
        ks = sorted(dd)
        tps0 = dd[0]["decode_tps"]
        model = dd[0]["model_mb"] if "model_mb" in dd[0] else None
        # 未锁定字节比例：locked/model 从 trace 缺省按 budget 比例近似
        hidden = []
        gains = []
        for k in ks:
            if k == 0:
                hidden.append(0.0)
                gains.append(0.0)
                continue
            pf = dd[k].get("prefetch_bytes_per_step")
            unlocked = (dd[k].get("model_bytes", 0) or 0) - (dd[k].get("locked_bytes", 0) or 0)
            if pf and unlocked > 0:
                hidden.append(100.0 * pf / unlocked)
            else:
                # 回退：窗口比例 k/n_layers
                n_layers = 28 if "1.5B" in label else 36
                hidden.append(100.0 * k / n_layers)
            gains.append(100.0 * (dd[k]["decode_tps"] - tps0) / tps0)
        ax.plot(hidden, gains, "o-", color=c, lw=1.7, label=label)
    ax.axhline(0, color="0.5", lw=0.8)
    ax.set_xlabel("IO hidden by prefetch (% of unlocked pages)")
    ax.set_ylabel("throughput gain vs k=0 (%)")
    ax.set_title("prefetch overlap: IO hidden vs latency gain")
    ax.legend(frameon=True, fontsize=8)
    fig.savefig(FIG / "fig14_io_overlap.png")
    plt.close(fig)


def fig15_moe_prefetch():
    """MoE 专家预取策略对比（30% 预算，热点专家锁定）"""
    base = first(load("membudget_olmoe.json"))
    variants = {
        "lock only (base)": base,
        "+ top-16 static": first(load("membudget_olmoe_pf.json")),
        "+ cold rank 8-16": first(load("membudget_olmoe_pf_cold.json")),
        "+ dynamic (prev tok)": first(load("membudget_olmoe_pf_dyn_lo.json")),
    }
    labels = list(variants)
    tps = [variants[l]["decode_tps"] for l in labels]
    tpot = [variants[l]["tpot_mean_ms"] for l in labels]
    pf = [variants[l]["total_pfaults"] / 1e6 for l in labels]
    fig, axes = plt.subplots(1, 3, figsize=(11.0, 3.0))
    colors = [PALETTE[2], PALETTE[0], PALETTE[0], PALETTE[3]]
    axes[0].bar(labels, tps, color=colors, width=0.55)
    axes[0].set_xticks(range(len(labels)))
    axes[0].set_xticklabels(labels, rotation=18, ha="right", fontsize=7.4)
    axes[0].set_ylabel("decode tok/s"); axes[0].set_title("(a) throughput")
    axes[1].bar(labels, tpot, color=colors, width=0.55)
    axes[1].set_xticks(range(len(labels)))
    axes[1].set_xticklabels(labels, rotation=18, ha="right", fontsize=7.4)
    axes[1].set_ylabel("TPOT (ms)"); axes[1].set_title("(b) latency")
    axes[2].bar(labels, pf, color=colors, width=0.55)
    axes[2].set_xticks(range(len(labels)))
    axes[2].set_xticklabels(labels, rotation=18, ha="right", fontsize=7.4)
    axes[2].set_ylabel("page faults (M)"); axes[2].set_title("(c) swap-out IO")
    axes[0].annotate("prefetch thread\ncontention > overlap",
                     xy=(0.5, 0.92), xycoords="axes fraction", ha="center",
                     fontsize=8.2, style="italic", color=PALETTE[3])
    fig.tight_layout()
    fig.savefig(FIG / "fig15_moe_prefetch.png")
    plt.close(fig)


def main():
    fig13_prefetch_window()
    fig14_io_overlap()
    fig15_moe_prefetch()
    print("phase3 figures written to", FIG)


if __name__ == "__main__":
    main()
