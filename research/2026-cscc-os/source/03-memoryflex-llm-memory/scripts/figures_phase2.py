"""阶段2学术风格图件生成（内存预算/按需加载/换出）

读取 results/membudget_*.json，输出：
  fig8_budget_control.png    预算控制：锁定/常驻 vs 预算
  fig9_mem_perf_tradeoff.png 内存-吞吐权衡（帕累托）
  fig10_swapout_io.png       换出代价：缺页 vs 预算
  fig11_moe_expert_lock.png  MoE 页级热点专家锁定收益
  fig12_kv_quant.png         KV 精度/上下文内存实测

用法: python scripts/figures_phase2.py
"""

import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np


ROOT = Path(__file__).resolve().parent.parent
RES = ROOT / "results"
FIG = ROOT / "docs/figures"

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


def fig8_budget_control(d15, d3):
    """锁定/常驻内存 vs 预算：预算控制器有效性"""
    fig, axes = plt.subplots(1, 2, figsize=(9.2, 3.1))
    for ax, d, label in [(axes[0], d15, "Qwen2.5-1.5B"),
                         (axes[1], d3, "Qwen2.5-3B")]:
        s = d["sweep"]
        pct = [x["pct"] for x in s]
        budget = [x["budget_mb"] for x in s]
        locked = [x.get("locked_mb", 0) for x in s]
        rss = [x.get("mean_rss_mb", 0) for x in s]
        ax.plot(pct, budget, "--", color="0.45", lw=1.0, label="budget (target)")
        ax.plot(pct, locked, "o-", color=PALETTE[0], lw=1.6, label="locked (resident set)")
        ax.plot(pct, rss, "s--", color=PALETTE[3], lw=1.3, label="mean working set")
        ax.set_xlabel("memory budget (% of model)")
        ax.set_ylabel("MB")
        ax.set_title(label, fontsize=10)
        ax.legend(frameon=True, fontsize=7.4)
    fig.savefig(FIG / "fig8_budget_control.png")
    plt.close(fig)


def fig9_mem_perf_tradeoff(d15, d3):
    """内存-吞吐权衡：更小预算 = 更小常驻 + 更低吞吐"""
    fig, ax = plt.subplots(figsize=(5.6, 3.1))
    for d, label, c in [(d15, "Qwen2.5-1.5B", PALETTE[0]),
                        (d3, "Qwen2.5-3B", PALETTE[1])]:
        s = d["sweep"]
        rss = [x.get("mean_rss_mb", 0) for x in s]
        tps = [x.get("decode_tps", 0) for x in s]
        ax.plot(rss, tps, "o-", color=c, lw=1.6, label=label)
        ax.annotate(f"{s[0]['pct']}%", xy=(rss[0], tps[0]), xytext=(rss[0] - 60, tps[0] + 4),
                    fontsize=8, color=c)
        ax.annotate(f"{s[-1]['pct']}%", xy=(rss[-1], tps[-1]), xytext=(rss[-1] + 8, tps[-1] - 5),
                    fontsize=8, color=c)
    ax.set_xlabel("mean working set (MB)")
    ax.set_ylabel("decode throughput (tok/s)")
    ax.set_title("memory–performance Pareto frontier")
    ax.legend(frameon=True, fontsize=8)
    fig.savefig(FIG / "fig9_mem_perf_tradeoff.png")
    plt.close(fig)


def fig10_swapout_io(d15, d3):
    """换出代价：缺页次数随预算收紧指数上升"""
    fig, ax = plt.subplots(figsize=(5.6, 3.1))
    for d, label, c in [(d15, "Qwen2.5-1.5B", PALETTE[0]),
                        (d3, "Qwen2.5-3B", PALETTE[1])]:
        s = d["sweep"]
        pct = [x["pct"] for x in s]
        pf = [x.get("total_pfaults", 0) / 1e6 for x in s]
        ax.plot(pct, pf, "o-", color=c, lw=1.6, label=label)
    ax.set_yscale("log")
    ax.set_xlabel("memory budget (% of model)")
    ax.set_ylabel("page faults (millions)")
    ax.set_title("swap-out IO cost vs budget")
    ax.legend(frameon=True, fontsize=8)
    fig.savefig(FIG / "fig10_swapout_io.png")
    plt.close(fig)


def fig11_moe_expert_lock(d_ol, d_ol_nl):
    """MoE 页级热点专家锁定：锁定集合/吞吐/缺页对比"""
    s = d_ol["sweep"]; s_nl = d_ol_nl["sweep"]
    fig, axes = plt.subplots(1, 3, figsize=(10.5, 3.0))
    x = np.arange(len(s))
    locked_l = [v.get("locked_mb", 0) for v in s]
    locked_nl = [v.get("locked_mb", 0) for v in s_nl]
    tps_l = [v.get("decode_tps", 0) for v in s]
    tps_nl = [v.get("decode_tps", 0) for v in s_nl]
    pf_l = [v.get("total_pfaults", 0) / 1e6 for v in s]
    pf_nl = [v.get("total_pfaults", 0) / 1e6 for v in s_nl]
    w = 0.36
    axes[0].bar(x - w / 2, locked_l, w, color=PALETTE[0], label="hot-expert lock")
    axes[0].bar(x + w / 2, locked_nl, w, color=PALETTE[3], label="tensor-only")
    axes[0].set_xticks(x); axes[0].set_xticklabels([f"{v['pct']}%" for v in s])
    axes[0].set_xlabel("budget"); axes[0].set_ylabel("locked MB")
    axes[0].set_title("(a) resident lock set")
    axes[0].legend(frameon=True, fontsize=7.2)
    axes[1].bar(x - w / 2, tps_l, w, color=PALETTE[0])
    axes[1].bar(x + w / 2, tps_nl, w, color=PALETTE[3])
    axes[1].set_xticks(x); axes[1].set_xticklabels([f"{v['pct']}%" for v in s])
    axes[1].set_xlabel("budget"); axes[1].set_ylabel("decode tok/s")
    axes[1].set_title("(b) throughput")
    axes[1].annotate(f"+{100*(sum(tps_l)/len(tps_l) - sum(tps_nl)/len(tps_nl)) / (sum(tps_nl)/len(tps_nl)):.0f}%",
                     xy=(0.5, 0.9), xycoords="axes fraction", ha="center", fontsize=9,
                     color=PALETTE[0], fontweight="bold")
    axes[2].bar(x - w / 2, pf_l, w, color=PALETTE[0], label="hot-expert lock")
    axes[2].bar(x + w / 2, pf_nl, w, color=PALETTE[3])
    axes[2].set_xticks(x); axes[2].set_xticklabels([f"{v['pct']}%" for v in s])
    axes[2].set_xlabel("budget"); axes[2].set_ylabel("page faults (M)")
    axes[2].set_title("(c) swap-out IO")
    fig.tight_layout()
    fig.savefig(FIG / "fig11_moe_expert_lock.png")
    plt.close(fig)


def fig12_kv_quant(d15):
    """KV 精度与上下文长度：实测与理论曲线"""
    kv = d15.get("kv_sweep", [])
    fig, ax = plt.subplots(figsize=(5.6, 3.0))
    base = min(x["peak_rss_mb"] for x in kv)
    for k, c, m in [("f16", PALETTE[0], "o"), ("q8_0", PALETTE[1], "s")]:
        rows = [x for x in kv if x["kv"] == k]
        ax.plot([r["ctx"] for r in rows], [r["peak_rss_mb"] for r in rows],
                f"{m}-", color=c, lw=1.6, label=f"measured KV {k}")
        # 理论增长斜率：kv/token f16=14336B, q8=7168B
        slope = 14336 / 1e6 if k == "f16" else 7168 / 1e6
        ctx = np.array([512, 4096])
        ax.plot(ctx, base + slope * ctx, ":", color=c, lw=1.0)
    ax.set_xlabel("context length (tokens)")
    ax.set_ylabel("peak resident memory (MB)")
    ax.set_title("KV quantization vs context (1.5B)")
    ax.legend(frameon=True, fontsize=8)
    fig.savefig(FIG / "fig12_kv_quant.png")
    plt.close(fig)


def main():
    d15 = load("membudget_1.5b.json")
    d3 = load("membudget_3b.json")
    dol = load("membudget_olmoe.json")
    dolnl = load("membudget_olmoe_nolock.json")
    fig8_budget_control(d15, d3)
    fig9_mem_perf_tradeoff(d15, d3)
    fig10_swapout_io(d15, d3)
    fig11_moe_expert_lock(dol, dolnl)
    fig12_kv_quant(d15)
    print("phase2 figures written to", FIG)


if __name__ == "__main__":
    main()
