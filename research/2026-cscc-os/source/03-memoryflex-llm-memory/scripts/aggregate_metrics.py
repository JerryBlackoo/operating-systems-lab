"""统一指标汇总与追踪（阶段5：科学严谨的评测）

扫描 results/ 下全部实验 JSON，提取统一指标：
  - 延迟：TTFT/TPOT（P50/P95/P99）
  - 吞吐：decode tok/s
  - 内存：峰值/均值 RSS、锁定 MB、KV
  - IO：缺页次数
  - 调度：KV 命中率/命中 token、负载均衡 σ、告警数
  - SLO 满足率（TTFT<500ms、TPOT<250ms，由 detail 计算）
输出 results/metrics_tracking.csv（逐实验一行）与 results/metrics_summary.json。

用法: python scripts/aggregate_metrics.py
"""

import csv
import json
import statistics
from pathlib import Path


RES = Path(__file__).resolve().parent.parent / "results"
SLO_TTFT_MS = 500
SLO_TPOT_MS = 250


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return None
    k = (len(xs) - 1) * p
    f = int(k)
    c = min(f + 1, len(xs) - 1)
    return round(xs[f] + (xs[c] - xs[f]) * (k - f), 1)


def slo_from_detail(path, tag):
    d = json.loads(path.read_text(encoding="utf-8"))
    ttft = [r["ttft_ms"] for r in d if r.get("ttft_ms")]
    tpot = [r["tpot_mean_ms"] for r in d if r.get("tpot_mean_ms")]
    row = {
        "experiment": tag, "source": path.name,
        "n_requests": len(d),
        "ttft_mean_ms": round(statistics.mean(ttft), 1) if ttft else None,
        "ttft_p50_ms": pct(ttft, 0.5),
        "ttft_p95_ms": pct(ttft, 0.95),
        "ttft_p99_ms": pct(ttft, 0.99),
        "tpot_mean_ms": round(statistics.mean(tpot), 2) if tpot else None,
        "tpot_p95_ms": pct(tpot, 0.95),
        "slo_ttft_rate": round(sum(1 for t in ttft if t < SLO_TTFT_MS) / len(ttft), 3) if ttft else None,
        "slo_tpot_rate": round(sum(1 for t in tpot if t < SLO_TPOT_MS) / len(tpot), 3) if tpot else None,
        "kv_hit_rate": round(sum(1 for r in d if r.get("hit_len", 0) // 4 > 16) / len(d), 3) if d else None,
        "kv_hit_tokens_mean": round(statistics.mean([r["hit_len"] // 4 for r in d]), 1) if d else None,
    }
    return row


def membudget_rows():
    rows = []
    for f in sorted(RES.glob("membudget_*.json")):
        if "cluster" in f.name:
            continue
        try:
            d = json.loads(f.read_text(encoding="utf-8"))
        except Exception:
            continue
        for s in d.get("sweep", []):
            rows.append({
                "experiment": f"membudget_{d.get('model_id')}", "source": f.name,
                "model": d.get("model_id"), "budget_pct": s.get("pct"),
                "budget_mb": s.get("budget_mb"), "locked_mb": s.get("locked_mb"),
                "mean_rss_mb": s.get("mean_rss_mb"), "peak_rss_mb": s.get("peak_rss_mb"),
                "total_pfaults": s.get("total_pfaults"),
                "decode_tps": s.get("decode_tps"), "tpot_mean_ms": s.get("tpot_mean_ms"),
                "ttft_mean_ms": s.get("ttft_mean_ms"),
            })
    return rows


def cluster_rows():
    rows = []
    for pol in ("lb", "lmetric", "linear"):
        f = RES / f"cluster_{pol}.json"
        if not f.exists():
            continue
        s = json.loads(f.read_text(encoding="utf-8"))
        rows.append({
            "experiment": f"cluster_{pol}", "source": f.name,
            "policy": pol, "n_requests": s.get("n_requests"),
            "ttft_mean_ms": s.get("ttft_mean_ms"), "ttft_p95_ms": s.get("ttft_p95_ms"),
            "tpot_mean_ms": s.get("tpot_mean_ms"),
            "kv_hit_rate": s.get("kv_hit_rate"),
            "load_balance_std": s.get("load_balance_std"),
            "n_alarms": s.get("n_alarms"), "n_failures": s.get("n_failures"),
        })
    for tag in ("cluster_hotspot_lm", "cluster_hotspot_lm_det"):
        f = RES / f"{tag}.json"
        if f.exists():
            s = json.loads(f.read_text(encoding="utf-8"))
            rows.append({
                "experiment": tag, "source": f.name, "policy": "lmetric(+det)",
                "n_requests": s.get("n_requests"),
                "ttft_mean_ms": s.get("ttft_mean_ms"), "tpot_mean_ms": s.get("tpot_mean_ms"),
                "kv_hit_rate": s.get("kv_hit_rate"),
                "load_balance_std": s.get("load_balance_std"),
                "n_alarms": s.get("n_alarms"),
            })
    return rows


def baseline_rows():
    rows = []
    for f in sorted(RES.glob("qwen2.5-*_ngl*.json")):
        try:
            d = json.loads(f.read_text(encoding="utf-8"))
        except Exception:
            continue
        m = d.get("meta", {})
        tpot = (d.get("latency", {}).get("tpot_ms") or {}).get("mean")
        if tpot is not None and tpot < 1.0:
            continue  # 早期流式计时 bug 的数据（TPOT 失真），不计入
        rows.append({
            "experiment": f"baseline_{m.get('model_id')}_ngl{m.get('gpu_layers')}",
            "source": f.name, "model": m.get("model_id"),
            "ttft_mean_ms": (d.get("latency", {}).get("ttft_ms") or {}).get("mean"),
            "tpot_mean_ms": (d.get("latency", {}).get("tpot_ms") or {}).get("mean"),
            "decode_tps": (d.get("throughput", {}).get("decode_tokens_per_s") or {}).get("mean"),
            "mean_rss_mb": (d.get("memory") or {}).get("rss_mean_mb"),
            "gpu_peak_mb": (d.get("memory") or {}).get("gpu_peak_mb"),
        })
    return rows


def main():
    rows = baseline_rows() + membudget_rows() + cluster_rows()
    # detail 文件（SLO）
    for f in sorted(RES.glob("cluster_*.detail.json")):
        rows.append(slo_from_detail(f, f.name.replace(".detail.json", "")))
    # 稳定性
    sf = RES / "stability.json"
    if sf.exists():
        s = json.loads(sf.read_text(encoding="utf-8"))
        rows.append({"experiment": "stability_95s", "source": sf.name,
                     "duration_s": s.get("duration_s"), "rss_mean_mb": s.get("rss_mean_mb"),
                     "rss_growth_mb": s.get("rss_growth_mb"),
                     "threads_max": s.get("threads_max"), "ttft_mean_ms": s.get("ttft_mean_ms")})

    fieldnames = sorted({k for r in rows for k in r})
    with open(RES / "metrics_tracking.csv", "w", newline="", encoding="utf-8-sig") as f:
        w = csv.DictWriter(f, fieldnames=fieldnames)
        w.writeheader()
        w.writerows(rows)
    (RES / "metrics_summary.json").write_text(
        json.dumps(rows, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"aggregated {len(rows)} metric rows -> results/metrics_tracking.csv")


if __name__ == "__main__":
    main()
