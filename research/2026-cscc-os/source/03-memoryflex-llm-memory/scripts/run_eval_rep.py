"""关键对比实验重复验证（阶段5：科学严谨）

对集群调度关键对比（LB vs LMETRIC）重复 N 次，报告 mean ± std，
并做配对显著性提示（配对 t 检验近似：以差值符号与幅度说明）。

用法: python scripts/run_eval_rep.py --trace results/trace_chat.jsonl \
      --model models/xx.gguf --model-mb 1111 --instances 4 --budget-pct 60 \
      --concurrency 2 --reps 3 --out results/repeat_cluster.json
"""

import argparse
import json
import statistics
from pathlib import Path

from router_lmetric import run_experiment


class Args:
    pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trace", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--model-mb", type=float, required=True)
    ap.add_argument("--instances", type=int, default=4)
    ap.add_argument("--budget-pct", type=int, default=60)
    ap.add_argument("--concurrency", type=int, default=2)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--out", default="results/repeat_cluster.json")
    args = ap.parse_args()

    a = Args()
    a.trace = args.trace
    a.model = args.model
    a.model_mb = args.model_mb
    a.instances = args.instances
    a.budget_pct = args.budget_pct
    a.concurrency = args.concurrency
    a.cache_reuse = 32
    a.ctx = 2048
    a.gpu_layers = 0
    a.linear_alpha = 0.5
    a.port = 18500

    tmp = Path("results") / "_rep_tmp"
    tmp.mkdir(parents=True, exist_ok=True)
    results = {"reps": args.reps, "config": vars(args), "runs": {}}
    for pol in ("lb", "lmetric"):
        runs = []
        for i in range(args.reps):
            print(f"[{pol}] rep {i+1}/{args.reps} ...", flush=True)
            out = run_experiment(a, pol, tmp / f"rep_{pol}_{i}.json")
            runs.append({"ttft_mean_ms": out["ttft_mean_ms"],
                         "kv_hit_rate": out["kv_hit_rate"],
                         "load_balance_std": out["load_balance_std"],
                         "tpot_mean_ms": out["tpot_mean_ms"]})
            print("   ", runs[-1], flush=True)
        agg = {}
        for key in ("ttft_mean_ms", "kv_hit_rate", "load_balance_std", "tpot_mean_ms"):
            vals = [r[key] for r in runs if r[key] is not None]
            if vals:
                agg[key] = {"mean": round(statistics.mean(vals), 2),
                            "std": round(statistics.pstdev(vals), 2),
                            "runs": vals}
        results["runs"][pol] = {"aggregate": agg, "raw": runs}

    # 配对差值（LB - LMETRIC 的 TTFT）
    lb = [r["ttft_mean_ms"] for r in results["runs"]["lb"]["raw"]]
    lm = [r["ttft_mean_ms"] for r in results["runs"]["lmetric"]["raw"]]
    if lb and lm and len(lb) == len(lm):
        diffs = [b - m for b, m in zip(lb, lm)]
        results["paired_ttft_diff_lb_minus_lm"] = {
            "mean_ms": round(statistics.mean(diffs), 1),
            "all_positive": all(d > 0 for d in diffs),
            "note": "所有重复中 LMETRIC 的 TTFT 均低于 LB（差值同号），支持稳健收益" if all(d > 0 for d in diffs)
                    else "差值符号不完全一致，收益受运行噪声影响",
        }

    Path(args.out).write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    print("saved ->", args.out)


if __name__ == "__main__":
    main()
