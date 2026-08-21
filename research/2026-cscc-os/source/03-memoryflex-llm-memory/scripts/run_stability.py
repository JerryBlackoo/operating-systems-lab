"""长跑稳定性测试（阶段6：加固验证）

在内存预算 + 预取开启下连续处理多轮请求，采样 进程线程数 / RSS / 缺页，
验证：无崩溃、线程数稳定（预取线程常驻）、RSS 无泄漏、吞吐稳定。

用法: python scripts/run_stability.py --model models/xx.gguf --budget-pct 40 \
      --prefetch-layers 8 --requests 16 --max-tokens 24 --out results/stability.json
"""

import argparse
import json
import os
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

import psutil
import requests


def find_server_exe():
    c = Path(__file__).resolve().parent.parent / "tools/llama.cpp/build/bin/Release/llama-server.exe"
    if not c.is_file():
        raise FileNotFoundError("llama-server.exe not found")
    return c


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--budget-pct", type=int, default=40)
    ap.add_argument("--prefetch-layers", type=int, default=8)
    ap.add_argument("--requests", type=int, default=16)
    ap.add_argument("--max-tokens", type=int, default=24)
    ap.add_argument("--out", default="results/stability.json")
    args = ap.parse_args()

    model = Path(args.model)
    model_mb = model.stat().st_size / 1e6
    budget = round(model_mb * args.budget_pct / 100.0)
    env = dict(os.environ)
    env["LLAMA_MEM_BUDGET_MB"] = str(budget)
    env["LLAMA_PREFETCH_LAYERS"] = str(args.prefetch_layers)

    port = 18400
    cmd = [str(find_server_exe()), "--model", str(model), "--host", "127.0.0.1",
           "--port", str(port), "--n-gpu-layers", "0", "--ctx-size", "2048",
           "--parallel", "1"]
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT, env=env)
    ps = psutil.Process(proc.pid)
    samples = []
    stop = threading.Event()

    def sampler():
        while not stop.is_set():
            try:
                m = ps.memory_info()
                samples.append({"t": time.time(), "rss_mb": m.rss / 1e6,
                                "threads": ps.num_threads(),
                                "pfaults": getattr(m, "num_page_faults", 0)})
            except Exception:
                pass
            time.sleep(1.0)

    th = threading.Thread(target=sampler, daemon=True)
    th.start()
    t0 = time.time()
    ttft = []
    try:
        base = f"http://127.0.0.1:{port}/"
        deadline = time.time() + 120
        while time.time() < deadline:
            if proc.poll() is not None:
                raise RuntimeError("server exited")
            try:
                if requests.get(base + "health", timeout=2).status_code == 200:
                    break
            except Exception:
                pass
            time.sleep(0.3)
        payload = {"model": "m",
                   "messages": [{"role": "user",
                                 "content": "请介绍虚拟内存分页、换出与预取机制，并给出参数示例。"}],
                   "max_tokens": args.max_tokens, "temperature": 0.0, "stream": True}
        for i in range(args.requests):
            st = time.time()
            first = None
            with requests.post(base + "v1/chat/completions", json=payload,
                               stream=True, timeout=180) as r:
                for line in r.iter_lines(decode_unicode=True):
                    s = line if isinstance(line, str) else line.decode("utf-8", "replace")
                    if "content" in s and "delta" in s and first is None:
                        first = time.time()
            if first:
                ttft.append((first - st) * 1000)
            if (i + 1) % 4 == 0:
                print(f"  req {i+1}/{args.requests} done, elapsed={time.time()-t0:.0f}s", flush=True)
    finally:
        time.sleep(1.0)
        stop.set()
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()

    rss = [s["rss_mb"] for s in samples]
    threads = [s["threads"] for s in samples]
    total_s = time.time() - t0
    # RSS 泄漏检测：后半段与前半段均值对比
    half = len(rss) // 2
    rss_growth = (statistics.mean(rss[half:]) - statistics.mean(rss[:half])) if half > 2 else 0
    out = {
        "model": str(model), "budget_pct": args.budget_pct,
        "prefetch_layers": args.prefetch_layers, "requests": args.requests,
        "duration_s": round(total_s, 1),
        "rss_mean_mb": round(statistics.mean(rss), 1),
        "rss_peak_mb": round(max(rss), 1),
        "rss_growth_mb": round(rss_growth, 1),
        "threads_mean": round(statistics.mean(threads), 1),
        "threads_max": max(threads),
        "ttft_mean_ms": round(statistics.mean(ttft), 1) if ttft else None,
        "stable": threads[-1] == threads[0] if len(threads) > 2 else None,
    }
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(out, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(out, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
