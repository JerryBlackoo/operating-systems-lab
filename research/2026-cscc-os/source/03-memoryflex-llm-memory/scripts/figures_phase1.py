"""阶段1学术风格图件生成（统一风格，每图一个核心结论）

读取 docs/figures/*.json 与 results/olmoe_moe_trace.bin 的分析结果，
输出 7 张期刊风格图（200 dpi，serif 字体，颜色友好调色板）：
  fig1_param_access.png   参数组成：每 token 访存 = 模型大小
  fig2_prefetch_window.png  预取窗口 k 控制常驻内存（k/n 压缩）
  fig3_mmap_timeline.png    mmap 按需加载 vs 全量读入
  fig4_kv_scaling.png       KV 线性增长 vs 显存预算
  fig5_prefix_cache.png     前缀缓存命中率与命中长度分布
  fig6_prefix_benefit.png   前缀命中实测 TTFT 收益
  fig7_moe_activation.png   专家激活稀疏性与热点集中（Lorenz）

用法: python scripts/figures_phase1.py
"""

import json
import struct
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np


FIG_DIR = Path(__file__).resolve().parent.parent / "docs/figures"

PALETTE = ["#4C72B0", "#DD8452", "#55A868", "#C44E52", "#8172B3", "#937860", "#64B5CD"]

plt.rcParams.update({
    "font.family": "serif",
    "font.serif": ["Times New Roman", "DejaVu Serif"],
    "font.size": 9.5,
    "axes.titlesize": 10.5,
    "axes.labelsize": 10,
    "xtick.labelsize": 8.5,
    "ytick.labelsize": 8.5,
    "legend.fontsize": 8.5,
    "axes.spines.top": False,
    "axes.spines.right": False,
    "axes.grid": True,
    "grid.alpha": 0.25,
    "grid.linewidth": 0.6,
    "figure.dpi": 200,
    "savefig.bbox": "tight",
})


def load(name):
    with open(FIG_DIR / name, encoding="utf-8") as f:
        return json.load(f)


def load_full_expert_freq(stats, trace_path=None):
    """优先从轨迹二进制计算完整 层×专家 激活频次；缺失时回退到统计 JSON 的 top-5。"""
    n_layers, n_exp = stats["n_layers"], stats["n_expert"]
    H = np.zeros((n_layers, n_exp))
    if trace_path is not None and Path(trace_path).is_file():
        n_used = stats["n_expert_used_per_token"]
        with open(trace_path, "rb") as f:
            while True:
                hdr = f.read(12)
                if len(hdr) < 12:
                    break
                layer, n_e, n_t = struct.unpack("<iii", hdr)
                raw = f.read(4 * n_e * n_t)
                if len(raw) < 4 * n_e * n_t:
                    break
                if n_t != 1:
                    continue
                probs = np.frombuffer(raw, dtype="<f4")
                top = np.argsort(-probs)[:n_used]
                for e in top:
                    H[layer, int(e)] += 1
        return H
    for l, top in stats["layer_top_experts"].items():
        for e, c in dict(top).items():
            H[int(l), int(e)] = c
    return H


def annotate(ax, x, y, text, dx=0, dy=0, **kw):
    ax.annotate(text, xy=(x, y), xytext=(x + dx, y + dy),
                fontsize=8.2, ha="center", **kw)


def fig1_param_access(models):
    """Fig.1 权重组成：dense 与 MoE 的每 token 参数访问量"""
    names = ["Qwen2.5-1.5B", "Qwen2.5-3B", "OLMoE-1B-7B"]
    emb = [models[n]["embedding_bytes_mb"] for n in names]
    attn = [models[n]["attention_bytes_mb"] for n in names]
    ffn = [models[n]["ffn_bytes_mb"] for n in names]
    exp = [models[n].get("expert_bytes_mb", 0) for n in names]
    total = [models[n]["total_bytes_mb"] for n in names]
    y = np.arange(len(names))

    fig, ax = plt.subplots(figsize=(5.4, 3.0))
    ax.barh(y, emb, color=PALETTE[0], label="Embedding / output")
    ax.barh(y, attn, left=emb, color=PALETTE[1], label="Attention")
    left = [a + b for a, b in zip(emb, attn)]
    ax.barh(y, ffn, left=left, color=PALETTE[2], label="FFN (dense / router)")
    left2 = [a + b + c for a, b, c in zip(emb, attn, ffn)]
    ax.barh(y, exp, left=left2, color=PALETTE[3], label="Expert weights (MoE)")
    ax.set_yticks(y)
    ax.set_yticklabels(names)
    ax.set_xlabel("parameter size (MB)")
    ax.set_xlim(0, max(total) * 1.18)
    for i, t in enumerate(total):
        ax.text(t + 40, y[i], f"{t:.0f} MB", va="center", fontsize=8.5, color="black")
    ax.text(0.99, 0.03,
            "per-token read = full parameter set\n(no temporal reuse)",
            transform=ax.transAxes, ha="right", fontsize=8.0, style="italic",
            bbox=dict(boxstyle="round,pad=0.35", fc="#FFF7E6", ec="#D9B34F", lw=0.8))
    ax.legend(loc="lower right", frameon=True, fontsize=7.6)
    fig.savefig(FIG_DIR / "fig1_param_access.png")
    plt.close(fig)


def fig2_prefetch_window(models):
    """Fig.2 预取窗口 k 决定常驻内存（FlexInfer k/n 压缩比）"""
    names = ["Qwen2.5-1.5B", "Qwen2.5-3B", "OLMoE-1B-7B"]
    fig, ax = plt.subplots(figsize=(5.4, 3.0))
    for i, n in enumerate(names):
        w = models[n]["prefetch_window_mb"]
        tot = models[n]["total_bytes_mb"]
        ks = [int(k) for k in w]
        pct = [100 * v / tot for v in w.values()]
        ax.plot(ks, pct, "o-", color=PALETTE[i], label=f"{n}")
        k3 = ks.index(3)
        ax.annotate(f"{pct[k3]:.0f}%", xy=(3, pct[k3]), xytext=(3.6, pct[k3] - 9),
                    fontsize=8.2, arrowprops=dict(arrowstyle="->", lw=0.8),
                    color=PALETTE[i])
    ax.axhline(100, ls="--", lw=0.9, color="0.5")
    ax.text(0.03, 0.965, "full residency", transform=ax.transAxes, fontsize=8,
            color="0.35", va="top")
    ax.set_xlabel("prefetch window $k$ (layers)")
    ax.set_ylabel("resident memory (% of model)")
    ax.set_xlim(0, max(int(k) for m in models.values() for k in m["prefetch_window_mb"]) * 1.05)
    ax.set_ylim(0, 115)
    ax.legend(frameon=True, fontsize=8)
    fig.savefig(FIG_DIR / "fig2_prefetch_window.png")
    plt.close(fig)


def fig3_mmap_timeline():
    """Fig.3 mmap 按需缺页加载 vs no-mmap 全量读入"""
    mm = load("mem_timeline_mmap.json")
    nm = load("mem_timeline_nommap.json")
    fig, axes = plt.subplots(1, 2, figsize=(9.0, 3.0))
    for ax, key, ylab in [(axes[0], "rss_mb", "RSS (MB)"),
                          (axes[1], "io_read_mb", "cumulative disk read (MB)")]:
        ax.plot([s["t"] for s in mm], [s[key] for s in mm], color=PALETTE[0],
                lw=1.6, label="mmap (on-demand)")
        ax.plot([s["t"] for s in nm], [s[key] for s in nm], color=PALETTE[3],
                lw=1.6, ls="--", label="no-mmap (eager)")
        ax.set_xlabel("time (s)")
        ax.set_ylabel(ylab)
    axes[0].annotate(f"peak RSS\n{max(s['rss_mb'] for s in mm):.0f} MB",
                     xy=(max(s["t"] for s in mm) * 0.75, max(s["rss_mb"] for s in mm) * 0.99),
                     fontsize=7.8, color=PALETTE[0])
    axes[1].annotate(f"{max(s['io_read_mb'] for s in nm):.0f} MB eager read",
                     xy=(0.5, 0.72), xycoords="axes fraction", fontsize=8, color=PALETTE[3],
                     ha="center")
    axes[0].legend(frameon=True, fontsize=7.6)
    fig.savefig(FIG_DIR / "fig3_mmap_timeline.png")
    plt.close(fig)


def fig4_kv_scaling(models):
    """Fig.4 KV 线性增长 vs 6GB 显存预算（KV 是受限资源）"""
    names = ["Qwen2.5-1.5B", "Qwen2.5-3B", "OLMoE-1B-7B"]
    ctx = np.array([512, 1024, 2048, 4096, 8192, 16384, 32768])
    fig, ax = plt.subplots(figsize=(5.4, 3.1))
    for i, n in enumerate(names):
        kv = models[n]["kv_bytes_per_token"]["f16"]
        ax.plot(ctx, kv * ctx / 1e6, "o-", color=PALETTE[i], label=f"{n} (f16)")
        kv8 = models[n]["kv_bytes_per_token"]["q8"]
        ax.plot(ctx, kv8 * ctx / 1e6, "--", color=PALETTE[i], lw=1.0, alpha=0.7)
    for mb, lab in [(1536, "1.5 GB (25% VRAM)"), (3072, "3.0 GB (50% VRAM)")]:
        ax.axhline(mb, ls=":", lw=0.9, color="0.45")
        ax.text(ctx[-1], mb + 25, lab, fontsize=7.6, color="0.35", ha="right")
    ax.set_xscale("log", base=2)
    ax.set_xticks(ctx)
    ax.set_xticklabels([str(c) for c in ctx], rotation=45, ha="right")
    ax.set_xlabel("context length (tokens)")
    ax.set_ylabel("KV cache size (MB)")
    ax.set_ylim(0, 3600)
    ax.legend(frameon=True, fontsize=7.4, ncol=1, loc="upper left")
    ax.text(0.99, 0.02, "KV competes with weights for\n6 GB GPU memory",
            transform=ax.transAxes, ha="right", fontsize=8.0, style="italic",
            bbox=dict(boxstyle="round,pad=0.35", fc="#FFF7E6", ec="#D9B34F", lw=0.8))
    fig.savefig(FIG_DIR / "fig4_kv_scaling.png")
    plt.close(fig)


def fig5_prefix_cache():
    """Fig.5 前缀缓存：命中率-容量曲线与命中长度分布"""
    d = load("kv_prefix_sim.json")
    sweep = d["sweep"]
    hits = d.get("hits_at_16k", [])
    fig, axes = plt.subplots(1, 2, figsize=(9.0, 3.0), gridspec_kw={"width_ratios": [1, 1.05]})
    xs = [s["capacity_tokens"] for s in sweep]
    ys = [s["hit_rate"] for s in sweep]
    axes[0].plot(xs, ys, "o-", color=PALETTE[0], lw=1.6)
    axes[0].set_xscale("log", base=2)
    axes[0].set_xticks(xs)
    axes[0].set_xticklabels([str(x) for x in xs], rotation=45, ha="right", fontsize=7.6)
    axes[0].set_xlabel("prefix-cache capacity (tokens)")
    axes[0].set_ylabel("prefix hit rate")
    axes[0].set_ylim(0, 0.6)
    axes[0].annotate("16K → 30.8%", xy=(16384, 0.308), xytext=(7000, 0.44),
                     fontsize=8, arrowprops=dict(arrowstyle="->", lw=0.8))
    axes[0].annotate("saturation ≈ 47%", xy=(65536, 0.475), xytext=(22000, 0.06),
                     fontsize=8, arrowprops=dict(arrowstyle="->", lw=0.8))
    axes[1].hist(hits, bins=28, color=PALETTE[1], edgecolor="white", lw=0.4)
    axes[1].set_xlabel("cached prefix tokens per request")
    axes[1].set_ylabel("request count")
    axes[1].annotate(f"multi-turn reuse:\nmedian hit ≈ {np.median(hits):.0f} tokens",
                     xy=(0.55, 0.88), xycoords="axes fraction", fontsize=8)
    fig.savefig(FIG_DIR / "fig5_prefix_cache.png")
    plt.close(fig)


def fig6_prefix_benefit():
    """Fig.6 前缀命中实测 TTFT 收益"""
    d = load("prefix_benefit.json")
    labels = ["cold (long)", "prefix hit", "control (short)"]
    vals = [x["ttft_ms"] for x in d]
    colors = [PALETTE[3], PALETTE[2], "0.65"]
    fig, ax = plt.subplots(figsize=(4.6, 2.8))
    bars = ax.bar(labels, vals, color=colors, width=0.55)
    for b, v in zip(bars, vals):
        ax.text(b.get_x() + b.get_width() / 2, v + 1.2, f"{v:.1f} ms",
                ha="center", fontsize=8.4)
    ax.annotate("", xy=(1, vals[1] + 2), xytext=(0, vals[0] + 2),
                arrowprops=dict(arrowstyle="<->", color="0.2", lw=1.0))
    ax.text(0.5, max(vals) + 6, "ΔTTFT = −33%", ha="center", fontsize=9,
            color=PALETTE[3], fontweight="bold")
    ax.set_ylabel("TTFT (ms)")
    ax.set_ylim(0, max(vals) * 1.35)
    fig.savefig(FIG_DIR / "fig6_prefix_benefit.png")
    plt.close(fig)


def fig7_moe_activation():
    """Fig.7 专家激活：稀疏性 + 热点集中（Lorenz）+ 层覆盖率"""
    d = load("moe_trace_stats.json")
    n_exp = d["n_expert"]
    freq = np.array(d["expert_freq"])
    conc = np.array(d["concentration"])
    cov = np.array(d["layer_coverage_pct"])
    top10 = d["activation_mass_top10_pct"]

    fig = plt.figure(figsize=(10.2, 3.2))
    gs = fig.add_gridspec(1, 3, width_ratios=[1.15, 1.15, 1.0])
    # (a) 层×专家 激活频次热图
    ax0 = fig.add_subplot(gs[0])
    trace = FIG_DIR.parent.parent / "results/olmoe_moe_trace.bin"
    H = load_full_expert_freq(d, trace)
    im = ax0.imshow(H, aspect="auto", cmap="magma", interpolation="nearest")
    ax0.set_xlabel("expert id"); ax0.set_ylabel("layer")
    ax0.set_title("(a) activation frequency", fontsize=9.5)
    fig.colorbar(im, ax=ax0, fraction=0.046, pad=0.03)
    # (b) 层覆盖率
    ax1 = fig.add_subplot(gs[1])
    x = np.arange(d["n_layers"])
    ax1.bar(x, cov, color=PALETTE[1], width=0.7)
    ax1.axhline(np.mean(cov), ls="--", lw=0.9, color="0.35")
    ax1.text(0, np.mean(cov) + 1.5, f"mean {np.mean(cov):.0f}%", fontsize=7.8, color="0.35")
    ax1.set_xlabel("layer"); ax1.set_ylabel("experts ever activated (%)")
    ax1.set_ylim(0, 100)
    ax1.set_title("(b) per-layer coverage", fontsize=9.5)
    # (c) Lorenz 浓度曲线
    ax2 = fig.add_subplot(gs[2])
    ax2.plot(np.arange(1, len(conc) + 1), conc * 100, color=PALETTE[0], lw=1.8)
    ax2.axhline(top10, ls=":", lw=0.9, color="0.5")
    ax2.plot([10], [top10], "o", color=PALETTE[3], ms=5)
    ax2.annotate(f"top-10 experts\n≈ {top10:.0f}% of activations",
                 xy=(10, top10), xytext=(16, top10 - 22), fontsize=8,
                 arrowprops=dict(arrowstyle="->", lw=0.8), color=PALETTE[3])
    ax2.set_xlabel("experts ranked by activation count")
    ax2.set_ylabel("cumulative activation mass (%)")
    ax2.set_xlim(0, 64); ax2.set_ylim(0, 100)
    ax2.set_title("(c) concentration (Lorenz)", fontsize=9.5)
    fig.tight_layout()
    fig.savefig(FIG_DIR / "fig7_moe_activation.png")
    plt.close(fig)


def main():
    models = {
        "Qwen2.5-1.5B": load("qwen2.5-1.5b-instruct-q4_k_m_gguf_analysis.json"),
        "Qwen2.5-3B": load("qwen2.5-3b-instruct-q4_k_m_gguf_analysis.json"),
        "OLMoE-1B-7B": load("OLMoE-1B-7B-0125-Q4_K_M_gguf_analysis.json"),
    }
    fig1_param_access(models)
    fig2_prefetch_window(models)
    fig3_mmap_timeline()
    fig4_kv_scaling(models)
    fig5_prefix_cache()
    fig6_prefix_benefit()
    fig7_moe_activation()
    print("figures written to", FIG_DIR)


if __name__ == "__main__":
    main()
