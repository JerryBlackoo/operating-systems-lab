"""基线指标采集 harness

启动 llama-server，发送流式 chat 请求，测量：
  - TTFT / TPOT (P50/P95/P99)
  - 解码吞吐 (tokens/s)
  - 进程常驻内存 (RSS) 峰值
  - GPU 显存峰值 (nvidia-smi 轮询, GPU 模式)
结果以 JSON 写入 --out-dir。

用法:
  python scripts/measure_baseline.py --model models/xx.gguf --model-id qwen2.5-1.5b \
      --gpu-layers 99 --ctx-size 2048 --n-requests 12 --max-tokens 64 --out-dir results
"""

import argparse
import json
import os
import re
import socket
import statistics
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

import psutil
import requests


PROMPT = (
    "请用中文简要介绍大语言模型推理的基本原理，包括预填充和解码两个阶段，"
    "以及 KV Cache 的作用。控制在两百字以内。"
)


def find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def find_server_exe() -> Path:
    candidates = [
        Path(os.environ.get("LLAMA_SERVER", "")),
        Path(__file__).resolve().parent.parent / "tools/llama.cpp/build/bin/Release/llama-server.exe",
        Path(__file__).resolve().parent.parent / "tools/llama.cpp/build/bin/llama-server.exe",
    ]
    for c in candidates:
        if c and c.is_file():
            return c
    raise FileNotFoundError("未找到 llama-server.exe；请先运行 scripts/build_llamacpp.ps1")


def parse_sse_lines(buf: str):
    for line in buf.splitlines():
        line = line.strip()
        if line.startswith("data:"):
            yield line[5:].strip()


class MetricsSampler(threading.Thread):
    """后台采样进程 RSS + GPU 显存"""

    def __init__(self, proc: psutil.Process, sample_gpu: bool, interval: float = 0.2):
        super().__init__(daemon=True)
        self.proc = proc
        self.sample_gpu = sample_gpu
        self.interval = interval
        self.rss = []
        self.vms = []
        self.gpu_mb = []
        self.stop = threading.Event()

    def run(self):
        while not self.stop.is_set():
            try:
                mem = self.proc.memory_info()
                self.rss.append(mem.rss)
                self.vms.append(mem.vms)
            except Exception:
                pass
            if self.sample_gpu:
                try:
                    out = subprocess.run(
                        ["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits"],
                        capture_output=True, text=True, timeout=2,
                    )
                    val = float(out.stdout.strip().splitlines()[0].strip())
                    self.gpu_mb.append(val)
                except Exception:
                    pass
            self.stop.wait(self.interval)

    def stop_now(self):
        self.stop.set()


def percentile(data, p):
    if not data:
        return 0.0
    data = sorted(data)
    k = (len(data) - 1) * p
    f = int(k)
    c = min(f + 1, len(data) - 1)
    return data[f] + (data[c] - data[f]) * (k - f)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--model-id", default="model")
    ap.add_argument("--gpu-layers", type=int, default=99)
    ap.add_argument("--ctx-size", type=int, default=2048)
    ap.add_argument("--n-requests", type=int, default=12)
    ap.add_argument("--max-tokens", type=int, default=64)
    ap.add_argument("--threads", type=int, default=0)
    ap.add_argument("--out-dir", default="results")
    ap.add_argument("--prompt", default=PROMPT)
    args = ap.parse_args()

    server = find_server_exe()
    port = find_free_port()
    log_path = Path(args.out_dir) / f"{args.model_id}_ngl{args.gpu_layers}_server.log"
    Path(args.out_dir).mkdir(parents=True, exist_ok=True)

    cmd = [
        str(server),
        "--model", args.model,
        "--host", "127.0.0.1",
        "--port", str(port),
        "--ctx-size", str(args.ctx_size),
        "--n-gpu-layers", str(args.gpu_layers),
        "--parallel", "1",
        "--log-file", str(log_path),
    ]
    if args.threads > 0:
        cmd += ["--threads", str(args.threads)]

    t0 = time.time()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    ps_proc = psutil.Process(proc.pid)
    sampler = None
    try:
        base = "http://127.0.0.1:{}/".format(port)
        # 等待 /health
        deadline = time.time() + 120
        while time.time() < deadline:
            if proc.poll() is not None:
                raise RuntimeError(f"llama-server 提前退出，rc={proc.returncode}，日志: {log_path}")
            try:
                r = requests.get(base + "health", timeout=2)
                if r.status_code == 200 and '"ok"' in r.text:
                    break
            except Exception:
                pass
            time.sleep(0.5)
        else:
            raise RuntimeError("llama-server 120s 内未就绪")
        startup_s = time.time() - t0

        sampler = MetricsSampler(ps_proc, sample_gpu=(args.gpu_layers > 0))
        sampler.start()

        url = base + "v1/chat/completions"
        payload = {
            "model": args.model_id,
            "messages": [{"role": "user", "content": args.prompt}],
            "max_tokens": args.max_tokens,
            "temperature": 0.0,
            "stream": True,
        }

        ttft_list, tpot_list, decode_tps_list, total_tps_list, out_tokens_list = [], [], [], [], []
        for i in range(args.n_requests):
            req_t0 = time.time()
            tok_times = []
            with requests.post(url, json=payload, stream=True, timeout=300) as resp:
                resp.raise_for_status()
                for line in resp.iter_lines(decode_unicode=True):
                    if not line:
                        continue
                    line = line.strip()
                    if not line.startswith("data:"):
                        continue
                    data = line[5:].strip()
                    if data == "[DONE]":
                        continue
                    try:
                        obj = json.loads(data)
                    except json.JSONDecodeError:
                        continue
                    delta = (obj.get("choices") or [{}])[0].get("delta") or {}
                    if delta.get("content"):
                        tok_times.append(time.time())
            if len(tok_times) == 0:
                print(f"[warn] 请求 {i} 无输出 token")
                continue
            ttft = (tok_times[0] - req_t0) * 1000
            tpot = [
                (tok_times[j] - tok_times[j - 1]) * 1000
                for j in range(1, len(tok_times))
            ]
            decode_s = (tok_times[-1] - tok_times[0])
            ttft_list.append(ttft)
            if tpot:
                tpot_list += tpot
            out_tokens_list.append(len(tok_times))
            if decode_s > 0:
                decode_tps_list.append((len(tok_times) - 1) / decode_s)
            total_tps_list.append(len(tok_times) / (tok_times[-1] - req_t0) if tok_times[-1] > req_t0 else 0)

        if sampler:
            time.sleep(0.5)
            sampler.stop_now()
            sampler.join(timeout=5)

        def stat(x):
            return {
                "mean": round(statistics.mean(x), 3) if x else None,
                "p50": round(percentile(x, 0.5), 3) if x else None,
                "p95": round(percentile(x, 0.95), 3) if x else None,
                "p99": round(percentile(x, 0.99), 3) if x else None,
            }

        result = {
            "meta": {
                "model_id": args.model_id,
                "model": str(Path(args.model).resolve()),
                "model_size_mb": round(Path(args.model).stat().st_size / 1e6, 1),
                "gpu_layers": args.gpu_layers,
                "ctx_size": args.ctx_size,
                "n_requests": args.n_requests,
                "max_tokens": args.max_tokens,
                "server_startup_s": round(startup_s, 2),
                "timestamp": datetime.now(timezone.utc).isoformat(),
            },
            "latency": {
                "ttft_ms": stat(ttft_list),
                "tpot_ms": stat(tpot_list),
            },
            "throughput": {
                "decode_tokens_per_s": stat(decode_tps_list),
                "total_tokens_per_s": stat(total_tps_list),
                "output_tokens_per_request": stat(out_tokens_list),
            },
            "memory": {
                "rss_peak_mb": round(max(sampler.rss) / 1e6, 1) if sampler and sampler.rss else None,
                "rss_mean_mb": round(statistics.mean(sampler.rss) / 1e6, 1) if sampler and sampler.rss else None,
                "gpu_peak_mb": round(max(sampler.gpu_mb), 1) if sampler and sampler.gpu_mb else None,
            },
        }

        out = Path(args.out_dir) / f"{args.model_id}_ngl{args.gpu_layers}_{int(time.time())}.json"
        out.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")

        print("===== Baseline result =====")
        print(f"model={args.model_id}  ngl={args.gpu_layers}  requests={args.n_requests}")
        print(f"TTFT (ms)   mean={result['latency']['ttft_ms']['mean']}  P50={result['latency']['ttft_ms']['p50']}  P95={result['latency']['ttft_ms']['p95']}  P99={result['latency']['ttft_ms']['p99']}")
        print(f"TPOT (ms)   mean={result['latency']['tpot_ms']['mean']}  P50={result['latency']['tpot_ms']['p50']}  P95={result['latency']['tpot_ms']['p95']}  P99={result['latency']['tpot_ms']['p99']}")
        print(f"decode tps  mean={result['throughput']['decode_tokens_per_s']['mean']}")
        print(f"mem peak    RSS={result['memory']['rss_peak_mb']} MB  GPU={result['memory']['gpu_peak_mb']} MB")
        print(f"startup     {result['meta']['server_startup_s']} s")
        print(f"saved to:   {out}")
    finally:
        if sampler:
            sampler.stop_now()
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()


if __name__ == "__main__":
    main()
