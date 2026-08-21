"""KV 前缀缓存收益实测（赛题任务1：KV cache 使用）

用 llama-server（--cache-reuse 开启前缀缓存）实测：
  A=长请求冷启动  B=共享前缀命中（同 system+user 前缀追加内容）  C=无关请求
对比 TTFT，量化前缀命中带来的预填充时间节省。

用法: python scripts/measure_prefix_benefit.py --model models/xx.gguf --out-dir docs/figures
"""

import argparse
import json
import subprocess
import time
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import requests


def ttft_of(port, messages, max_tokens=16):
    payload = {"model": "m", "messages": messages, "max_tokens": max_tokens,
               "temperature": 0.0, "stream": True}
    t0 = time.time()
    first = None
    r = requests.post(f"http://127.0.0.1:{port}/v1/chat/completions",
                      json=payload, stream=True, timeout=120)
    for line in r.iter_lines(decode_unicode=True):
        if not line or not line.startswith("data:"):
            continue
        data = line[5:].strip()
        if data == "[DONE]":
            break
        import json as _json
        try:
            obj = _json.loads(data)
            d = (obj.get("choices") or [{}])[0].get("delta") or {}
            if d.get("content") and first is None:
                first = time.time()
        except Exception:
            pass
    return (first - t0) * 1000 if first else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--out-dir", default="docs/figures")
    args = ap.parse_args()

    server = Path(__file__).resolve().parent.parent / "tools/llama.cpp/build/bin/Release/llama-server.exe"
    port = 18100
    cmd = [str(server), "--model", args.model, "--host", "127.0.0.1", "--port", str(port),
           "--n-gpu-layers", "99", "--ctx-size", "4096", "--parallel", "1", "--cache-reuse", "256"]
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    try:
        for _ in range(200):
            try:
                if requests.get(f"http://127.0.0.1:{port}/health", timeout=2).status_code == 200:
                    break
            except Exception:
                pass
            time.sleep(0.3)

        system = "你是一个严谨的嵌入式系统专家。请基于虚拟内存与缓存原理回答问题。"
        user_a = ("请解释大语言模型推理中 KV Cache 的作用、它随序列长度增长的内存占用规律，"
                  "以及为什么多轮对话场景下前缀缓存能够显著降低预填充开销。")
        user_b = user_a + " 另外请补充说明 GQA 多头分组注意力如何减少 KV Cache 的显存占用。"
        user_c = "请介绍乒乓球比赛的基本规则。"

        reqs = [
            ("A: cold (long)", [{"role": "system", "content": system},
                                {"role": "user", "content": user_a}]),
            ("B: prefix hit", [{"role": "system", "content": system},
                               {"role": "user", "content": user_b}]),
            ("C: unrelated", [{"role": "system", "content": system},
                              {"role": "user", "content": user_c}]),
        ]
        out = []
        for label, messages in reqs:
            ttft = ttft_of(port, messages)
            out.append({"request": label, "ttft_ms": ttft})
            print(f"{label}: TTFT={ttft:.1f} ms")
            time.sleep(1.0)

        out_dir = Path(args.out_dir)
        out_dir.mkdir(parents=True, exist_ok=True)
        (out_dir / "prefix_benefit.json").write_text(json.dumps(out, ensure_ascii=False, indent=2), encoding="utf-8")
        fig, ax = plt.subplots(figsize=(7, 4))
        labels = [x["request"] for x in out]
        vals = [x["ttft_ms"] or 0 for x in out]
        ax.bar(labels, vals, color=["#4c72b0", "#55a868", "#c44e52"])
        ax.set_ylabel("TTFT (ms)")
        ax.set_title("prefix-cache benefit (llama-server --cache-reuse)")
        for i, v in enumerate(vals):
            ax.text(i, v + 1, f"{v:.0f}", ha="center")
        fig.tight_layout()
        fig.savefig(out_dir / "prefix_benefit.png", dpi=120)
        plt.close(fig)
        print(f"saved -> {out_dir}/prefix_benefit.json / .png")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()


if __name__ == "__main__":
    main()
