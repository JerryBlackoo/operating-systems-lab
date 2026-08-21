"""GGUF 参数布局与访存模式分析（赛题任务1：参数加载）

基于 llama.cpp 官方 gguf-py 解析 GGUF 头部（不读取权重数据），输出：
  - 模型结构（层数、注意力头、KV 头、专家数等）
  - 张量清单（名称/字节大小），按 层/类别（attention/FFN/embedding/专家）汇总
  - 每 token 生成时参数访问特征：每个张量恰好访问一次、每 token 访存字节数
  - 预取窗口 k 对应的常驻内存（FlexInfer 的 k/n 压缩比）
  - KV cache 每 token 字节数与上下文长度曲线

用法: python scripts/analyze_gguf.py --model models/xx.gguf --out-dir docs/figures
"""

import argparse
import json
import sys
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tools/llama.cpp/gguf-py"))
from gguf import GGUFReader
from gguf.constants import GGUFValueType


def scalar(r, name):
    f = r.fields.get(name)
    if f is None:
        return None
    v = f.parts[f.data[0]]
    if f.types[0] == GGUFValueType.STRING:
        return bytes(v.tolist()).decode("utf-8", errors="replace")
    return v.item()


def classify(name):
    if name.startswith("blk."):
        layer = int(name.split(".")[1])
        if "attn" in name:
            kind = "attention"
        elif "ffn" in name:
            kind = "ffn"
        else:
            kind = "norm"
        return layer, kind
    return None, None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--out-dir", default="docs/figures")
    args = ap.parse_args()

    path = Path(args.model)
    r = GGUFReader(str(path))
    arch = scalar(r, "general.architecture")
    n_layer = int(scalar(r, f"{arch}.block_count") or 0)

    n_head = scalar(r, f"{arch}.attention.head_count")
    n_kv_head = scalar(r, f"{arch}.attention.head_count_kv") or n_head
    key_len = scalar(r, f"{arch}.attention.key_length")
    emb = scalar(r, f"{arch}.embedding_length")
    head_dim = key_len or (emb // n_head) if n_head else 0
    n_expert = scalar(r, f"{arch}.expert_count")
    n_expert_used = scalar(r, f"{arch}.expert_used_count")

    layer_sizes = {}
    attn_bytes = ffn_bytes = emb_bytes = other_bytes = 0
    expert_bytes = 0
    n_expert_tensors = 0
    total_bytes = 0
    for t in r.tensors:
        b = int(t.data.nbytes)
        total_bytes += b
        layer, kind = classify(t.name)
        if layer is None:
            if t.name in ("token_embd.weight", "output.weight", "output_norm.weight",
                          "token_embd_norm.weight"):
                emb_bytes += b
            else:
                other_bytes += b
            continue
        d = layer_sizes.setdefault(layer, {"attention": 0, "ffn": 0, "norm": 0, "experts": 0})
        if "_exps." in t.name:
            d["experts"] += b
            expert_bytes += b
            n_expert_tensors += 1
        else:
            d[kind] += b
            if kind == "attention":
                attn_bytes += b
            elif kind == "ffn":
                ffn_bytes += b

    n_layers = max(layer_sizes) + 1 if layer_sizes else 0
    if n_expert is None and n_expert_tensors:
        n_expert = max(n_expert_tensors // max(1, n_layers) // 3, 0)

    bytes_per_token = total_bytes
    layer_bytes = [sum(d.values()) for d in layer_sizes.values()]
    win = {}
    for k in sorted(set([1, 2, 3, 4, 8, n_layers])):
        win[k] = emb_bytes + sum(layer_bytes[:k])

    kv_f16 = 2 * n_layer * (n_kv_head or 1) * (head_dim or 0)
    kv_q8 = kv_f16 // 2
    ctx_pts = [256, 512, 1024, 2048, 4096, 8192, 16384]

    result = {
        "file": str(path), "file_size_mb": round(path.stat().st_size / 1e6, 1),
        "arch": arch, "n_layers": n_layers, "n_head": n_head,
        "n_kv_head": n_kv_head, "head_dim": head_dim, "embedding_length": emb,
        "n_expert": n_expert, "n_expert_used": n_expert_used,
        "n_tensors": len(r.tensors),
        "total_bytes_mb": round(total_bytes / 1e6, 1),
        "bytes_per_token_mb": round(bytes_per_token / 1e6, 2),
        "attention_bytes_mb": round(attn_bytes / 1e6, 1),
        "ffn_bytes_mb": round(ffn_bytes / 1e6, 1),
        "expert_bytes_mb": round(expert_bytes / 1e6, 1),
        "embedding_bytes_mb": round(emb_bytes / 1e6, 1),
        "per_layer_bytes_mb": [round(b / 1e6, 1) for b in layer_bytes],
        "prefetch_window_mb": {k: round(v / 1e6, 1) for k, v in win.items()},
        "kv_bytes_per_token": {"f16": kv_f16, "q8": kv_q8},
        "kv_mb_by_ctx": {"f16": {c: round(kv_f16 * c / 1e6, 1) for c in ctx_pts},
                         "q8": {c: round(kv_q8 * c / 1e6, 1) for c in ctx_pts}},
    }

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    json_path = out_dir / f"{path.stem}_gguf_analysis.json"
    json_path.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")

    fig, axes = plt.subplots(1, 3, figsize=(16, 4.5))
    attn = [layer_sizes[i]["attention"] / 1e6 for i in range(n_layers)]
    ffn = [layer_sizes[i]["ffn"] / 1e6 for i in range(n_layers)]
    experts = [layer_sizes[i]["experts"] / 1e6 for i in range(n_layers)]
    norm = [layer_sizes[i]["norm"] / 1e6 for i in range(n_layers)]
    axes[0].bar(range(n_layers), attn, label="attention")
    axes[0].bar(range(n_layers), ffn, bottom=attn, label="ffn")
    axes[0].bar(range(n_layers), experts, bottom=[a + b for a, b in zip(attn, ffn)], label="experts")
    axes[0].bar(range(n_layers), norm,
                bottom=[a + b + c for a, b, c in zip(attn, ffn, experts)], label="norm")
    axes[0].set_xlabel("layer"); axes[0].set_ylabel("MB")
    axes[0].set_title(f"{arch}: per-layer tensor size"); axes[0].legend()
    ks = list(win.keys()); vs = [win[k] / 1e6 for k in ks]
    axes[1].plot(ks, vs, "o-")
    axes[1].set_xlabel("prefetch window k (layers)")
    axes[1].set_ylabel("resident MB")
    axes[1].set_title("prefetch window footprint")
    axes[2].plot(ctx_pts, [kv_f16 * c / 1e6 for c in ctx_pts], "o-", label="KV f16")
    axes[2].plot(ctx_pts, [kv_q8 * c / 1e6 for c in ctx_pts], "s-", label="KV q8")
    axes[2].set_xlabel("context length (tokens)")
    axes[2].set_ylabel("KV MB")
    axes[2].set_title("KV cache size vs context"); axes[2].legend()
    fig.tight_layout()
    fig.savefig(out_dir / f"{path.stem}_gguf_analysis.png", dpi=120)
    plt.close(fig)

    print(f"=== {path.stem} ===")
    print(f"arch={arch} layers={n_layers} heads={n_head} kv_heads={n_kv_head} head_dim={head_dim} "
          f"expert={n_expert}/{n_expert_used}")
    print(f"tensors={len(r.tensors)} total={result['total_bytes_mb']} MB  "
          f"attn={result['attention_bytes_mb']} / ffn={result['ffn_bytes_mb']} / "
          f"experts={result['expert_bytes_mb']} / emb={result['embedding_bytes_mb']} MB")
    print(f"bytes/token={result['bytes_per_token_mb']} MB  (params read once per token)")
    print(f"prefetch window MB: {result['prefetch_window_mb']}")
    print(f"KV/token: f16={kv_f16} B, q8={kv_q8} B; ctx2048: f16={result['kv_mb_by_ctx']['f16'][2048]} MB")
    print(f"json -> {json_path}")


if __name__ == "__main__":
    main()
