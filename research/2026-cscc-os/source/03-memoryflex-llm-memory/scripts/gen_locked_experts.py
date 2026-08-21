"""生成热点专家锁定清单（阶段2：MoE 激活感知的按需加载）

根据阶段1的专家激活统计（moe_trace_stats.json 的 layer_top_experts），
对每层选 top-k 高频专家，输出页级专家锁定条目：
  blk.<层>.ffn_{gate,up,down}_exps.weight:<专家id逗号列表>
（OLMoE 等模型的专家权重为合并张量，按专家切片锁定字节区间）

用法: python scripts/gen_locked_experts.py --stats docs/figures/moe_trace_stats.json \
      --top-k 8 --out results/olmoe_hot_experts.txt
"""

import argparse
import json
from pathlib import Path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--stats", required=True)
    ap.add_argument("--top-k", type=int, default=8)
    ap.add_argument("--skip-k", type=int, default=0, help="跳过激活频次最高的前 N 个专家（生成冷专家预取清单）")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    stats = json.loads(Path(args.stats).read_text(encoding="utf-8"))
    names = []
    for layer, top in stats["layer_top_experts"].items():
        experts = [e for e, _ in top][args.skip_k:args.top_k]
        ids = ",".join(str(e) for e in sorted(experts))
        for part in ("ffn_gate_exps", "ffn_up_exps", "ffn_down_exps"):
            names.append(f"blk.{layer}.{part}.weight:{ids}")

    out = Path(args.out)
    out.write_text("# hot-expert lock list (Phase 2, activation-aware)\n" +
                   "\n".join(names) + "\n", encoding="utf-8")
    print(f"wrote {len(names)} page-level expert lock entries -> {out}")


if __name__ == "__main__":
    main()
