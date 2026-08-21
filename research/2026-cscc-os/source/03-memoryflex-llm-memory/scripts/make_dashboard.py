"""自包含 HTML 性能仪表盘（阶段5：可演示性）

将 docs/figures/ 图表与 results/metrics_summary.json 指标嵌入单个 HTML，
浏览器直接打开即可演示。输出 docs/性能仪表盘.html。

用法: python scripts/make_dashboard.py
"""

import base64
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
FIG = ROOT / "docs/figures"
RES = ROOT / "results"
OUT = ROOT / "docs/性能仪表盘.html"


def img64(name):
    p = FIG / name
    if not p.exists():
        return ""
    b64 = base64.b64encode(p.read_bytes()).decode()
    return f"data:image/png;base64,{b64}"


def table(rows, cols):
    head = "".join(f"<th>{c}</th>" for c in cols)
    body = ""
    for r in rows:
        body += "<tr>" + "".join(f"<td>{r.get(c, '')}</td>" for c in cols) + "</tr>"
    return f"<table><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>"


def main():
    summary = json.loads((RES / "metrics_summary.json").read_text(encoding="utf-8"))
    by_exp = {r["experiment"]: r for r in summary}

    # 关键表
    baseline_rows = [r for r in summary if r["experiment"].startswith("baseline_")]
    cluster_rows = [r for r in summary if r["experiment"].startswith("cluster_") and not r["experiment"].endswith("detail")]
    slo_rows = [r for r in summary if r["experiment"].endswith("detail")]
    stab = by_exp.get("stability_95s", {})

    html = f"""<!DOCTYPE html><html lang="zh"><head><meta charset="utf-8">
<title>内存受限环境的大模型推理优化 — 性能仪表盘</title>
<style>
body{{font-family:'Times New Roman', 'Songti SC', serif; margin:24px 40px; color:#222; background:#fff;}}
h1{{font-size:22px; border-bottom:2px solid #333; padding-bottom:6px;}}
h2{{font-size:16px; margin-top:28px; color:#1a3a6b;}}
h3{{font-size:13px; color:#444;}}
table{{border-collapse:collapse; font-size:12px; margin:10px 0;}}
th,td{{border:1px solid #ccc; padding:4px 9px; text-align:right;}}
th{{background:#f0f3f8; text-align:center;}}
td:first-child{{text-align:left;}}
img{{max-width:100%; margin:6px 0; border:1px solid #ddd;}}
.fig{{margin:14px 0;}}
.note{{font-size:12px; color:#555;}}
.kpi{{display:inline-block; margin:8px 18px 8px 0; font-size:14px;}}
.kpi b{{font-size:20px; color:#1a3a6b;}}
</style></head><body>
<h1>内存受限环境的大模型推理优化 — 性能评测仪表盘</h1>
<p class="note">引擎：llama.cpp commit 11924d4 + MemoryScheduler/预取补丁｜硬件：i9-13900HX / RTX 4050 6GB / 16GB｜模型：Qwen2.5-1.5B/3B、OLMoE-1B-7B（Q4_K_M）</p>

<h2>0. 关键指标速览</h2>
<div class="kpi">GPU 解码吞吐（1.5B）<br><b>131.8 tok/s</b></div>
<div class="kpi">40% 预算常驻压缩<br><b>1111→752 MB</b></div>
<div class="kpi">换出缺页代价<br><b>0.47M→17.3M</b></div>
<div class="kpi">预取窗口收益（3B）<br><b>+14%</b></div>
<div class="kpi">LMETRIC 命中率<br><b>+7.6%</b></div>
<div class="kpi">LMETRIC TTFT<br><b>−6.2%</b></div>
<div class="kpi">95s 长跑 RSS 增长<br><b>{stab.get('rss_growth_mb', '—')} MB</b></div>

<h2>1. 阶段 0 基线（满内存）</h2>
{table(baseline_rows, ["experiment", "ttft_mean_ms", "tpot_mean_ms", "decode_tps", "mean_rss_mb", "gpu_peak_mb"])}

<h2>2. 阶段 2：内存预算与换出</h2>
<div class="fig"><img src="{img64('fig8_budget_control.png')}"></div>
<div class="fig"><img src="{img64('fig9_mem_perf_tradeoff.png')}"></div>
<div class="fig"><img src="{img64('fig10_swapout_io.png')}"></div>

<h2>3. 阶段 3：异步预取</h2>
<div class="fig"><img src="{img64('fig13_prefetch_window.png')}"></div>
<div class="fig"><img src="{img64('fig14_io_overlap.png')}"></div>
<div class="fig"><img src="{img64('fig15_moe_prefetch.png')}"></div>

<h2>4. 阶段 4：LMETRIC 集群调度</h2>
<div class="fig"><img src="{img64('fig16_policy_compare.png')}"></div>
<div class="fig"><img src="{img64('fig17_kv_hit_mechanism.png')}"></div>
<div class="fig"><img src="{img64('fig18_hotspot_detector.png')}"></div>

<h2>5. SLO 满足率（TTFT&lt;500ms / TPOT&lt;250ms）</h2>
{table(slo_rows, ["experiment", "n_requests", "ttft_mean_ms", "ttft_p95_ms", "slo_ttft_rate", "slo_tpot_rate", "kv_hit_rate"])}

<h2>6. 访存行为分析（阶段 1）</h2>
<div class="fig"><img src="{img64('fig1_param_access.png')}"></div>
<div class="fig"><img src="{img64('fig2_prefetch_window.png')}"></div>
<div class="fig"><img src="{img64('fig7_moe_activation.png')}"></div>

<p class="note">完整指标追踪：results/metrics_tracking.csv；复现指南：docs/复现与评测指南.md；各阶段报告见 docs/。</p>
</body></html>"""
    OUT.write_text(html, encoding="utf-8")
    print(f"dashboard -> {OUT} ({OUT.stat().st_size/1024:.0f} KB)")


if __name__ == "__main__":
    main()
