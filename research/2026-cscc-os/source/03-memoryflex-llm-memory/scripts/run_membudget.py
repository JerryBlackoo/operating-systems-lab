"""阶段2：内存预算扫描实验（赛题任务2）

在 llama-server（CPU）上启用 MemoryScheduler（LLAMA_MEM_BUDGET_MB），
扫描不同内存预算（模型大小百分比），测量：
  - 峰值工作集（resident）是否 ≤ 预算
  - 锁定字节数（MemoryScheduler 平衡锁定）
  - TTFT/TPOT、解码吞吐（内存-性能权衡）
  - 缺页次数（换出导致的 IO 成本）

用法:
  python scripts/run_membudget.py --model models/xx.gguf --model-id 1.5b \
      --budgets 40,60,80,100 --requests 3 --max-tokens 32 --out-dir results
  可选: --lock-list results/olmoe_hot_experts.txt --kv-sweep
"""

import argparse
import json
import os
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


def percentile(data, p):
    data = sorted(data)
    if not data:
        return None
    k = (len(data) - 1) * p
    f = int(k)
    c = min(f + 1, len(data) - 1)
    return data[f] + (data[c] - data[f]) * (k - f)


class Sampler(threading.Thread):
    def __init__(self, proc):
        super().__init__(daemon=True)
        self.proc = proc
        self.samples = []
        self.stop = threading.Event()

    def run(self):
        prev_pf = None
        while not self.stop.is_set():
            try:
                m = self.proc.memory_info()
                pf = getattr(m, "num_page_faults", 0)
                delta = (pf - prev_pf) if prev_pf is not None else 0
                prev_pf = pf
                self.samples.append({"rss_mb": m.rss / 1e6, "vms_mb": m.vms / 1e6,
                                     "pfault_delta": delta})
            except Exception:
                pass
            self.stop.wait(0.1)


def run_one(server, model, model_id, budget_mb, lock_list, port, n_req, max_tok, n_expert=0,
            pf_layers=0, pf_list="", pf_experts=0):
    env = dict(os.environ)
    env["LLAMA_MEM_BUDGET_MB"] = str(budget_mb)
    if lock_list:
        env["LLAMA_MEM_LOCK_LIST"] = str(lock_list)
        if n_expert > 0:
            env["LLAMA_MEM_N_EXPERT"] = str(n_expert)
    if pf_layers > 0:
        env["LLAMA_PREFETCH_LAYERS"] = str(pf_layers)
    if pf_list:
        env["LLAMA_PREFETCH_LIST"] = str(pf_list)
    if pf_experts > 0:
        env["LLAMA_PREFETCH_EXPERTS"] = "1"
    env["LLAMA_MEM_TRACE"] = str(Path(lock_list).parent / f"ms_trace_{model_id}_{int(budget_mb)}.txt") \
        if lock_list else str(Path("results") / f"ms_trace_{model_id}_{int(budget_mb)}.txt")

    cmd = [str(server), "--model", str(model), "--host", "127.0.0.1", "--port", str(port),
           "--n-gpu-layers", "0", "--ctx-size", "2048", "--parallel", "1"]
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT, env=env)
    sampler = Sampler(psutil.Process(proc.pid))
    sampler.start()
    out = {"budget_mb": budget_mb, "ok": False}
    try:
        base = f"http://127.0.0.1:{port}/"
        deadline = time.time() + 180
        while time.time() < deadline:
            if proc.poll() is not None:
                return {**out, "error": f"server exited rc={proc.returncode}"}
            try:
                if requests.get(base + "health", timeout=2).status_code == 200:
                    break
            except Exception:
                pass
            time.sleep(0.4)
        else:
            return {**out, "error": "health timeout"}

        payload = {"model": model_id,
                   "messages": [{"role": "user",
                                 "content": "请详细解释虚拟内存分页机制与缺页中断的处理流程。"}],
                   "max_tokens": max_tok, "temperature": 0.0, "stream": True}
        ttft, tpot, tps = [], [], []
        for _ in range(n_req):
            t0 = time.time()
            toks = []
            with requests.post(base + "v1/chat/completions", json=payload,
                               stream=True, timeout=300) as r:
                for line in r.iter_lines(decode_unicode=True):
                    s = line if isinstance(line, str) else line.decode("utf-8", "replace")
                    if "content" in s and "delta" in s:
                        toks.append(time.time())
            if toks:
                ttft.append((toks[0] - t0) * 1000)
                d = toks[-1] - toks[0]
                if d > 0:
                    tps.append((len(toks) - 1) / d)
                tpot += [(b - a) * 1000 for a, b in zip(toks, toks[1:])]
        time.sleep(0.5)
        sampler.stop.set()
        sampler.join(timeout=5)
        rss = [s["rss_mb"] for s in sampler.samples]
        pfs = [s["pfault_delta"] for s in sampler.samples]
        out.update({
            "ok": True,
            "peak_rss_mb": round(max(rss), 1),
            "mean_rss_mb": round(sum(rss) / len(rss), 1),
            "total_pfaults": int(sum(pfs)),
            "ttft_mean_ms": round(sum(ttft) / len(ttft), 1) if ttft else None,
            "ttft_p95_ms": round(percentile(ttft, 0.95), 1) if ttft else None,
            "tpot_mean_ms": round(sum(tpot) / len(tpot), 2) if tpot else None,
            "decode_tps": round(sum(tps) / len(tps), 1) if tps else None,
        })
        # 读取 MemoryScheduler 锁定报告
        trace = Path(env["LLAMA_MEM_TRACE"])
        if trace.is_file():
            for line in trace.read_text(encoding="utf-8").splitlines():
                if line.startswith("budget_bytes="):
                    out["budget_bytes"] = int(line.split("=")[1])
                elif line.startswith("locked_bytes="):
                    out["locked_mb"] = round(int(line.split("=")[1]) / 1e6, 1)
        return out
    except Exception as e:
        return {**out, "error": str(e)}
    finally:
        sampler.stop.set()
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()


def kv_sweep(server, model, model_id, port):
    """KV 精度（f16/q8）与上下文长度的常驻内存实测。"""
    rows = []
    for ctx in (512, 2048, 4096):
        for kv in ("f16", "q8_0"):
            cmd = [str(server), "--model", str(model), "--host", "127.0.0.1", "--port", str(port),
                   "--n-gpu-layers", "0", "--ctx-size", str(ctx), "--parallel", "1",
                   "--cache-type-k", kv, "--cache-type-v", kv]
            proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
            ps = psutil.Process(proc.pid)
            peak = 0
            try:
                base = f"http://127.0.0.1:{port}/"
                deadline = time.time() + 60
                while time.time() < deadline:
                    try:
                        if requests.get(base + "health", timeout=2).status_code == 200:
                            break
                    except Exception:
                        pass
                    time.sleep(0.3)
                    try:
                        peak = max(peak, ps.memory_info().rss / 1e6)
                    except Exception:
                        pass
                time.sleep(0.5)
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    proc.kill()
            rows.append({"ctx": ctx, "kv": kv, "peak_rss_mb": round(peak, 1)})
            print(f"ctx={ctx} kv={kv} peak_rss={peak:.1f} MB")
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--model-id", required=True)
    ap.add_argument("--model-mb", type=float, required=True, help="模型大小(MB)，用于计算预算百分比")
    ap.add_argument("--budgets", default="40,60,80,100", help="预算百分比(逗号分隔)")
    ap.add_argument("--requests", type=int, default=3)
    ap.add_argument("--max-tokens", type=int, default=32)
    ap.add_argument("--lock-list", default="")
    ap.add_argument("--n-expert", type=int, default=0, help="MoE 专家数（页级专家锁定时必填）")
    ap.add_argument("--prefetch-layers", type=int, default=0, help="异步预取窗口 k（层数，0=关闭）")
    ap.add_argument("--prefetch-list", default="", help="MoE 专家预取清单（格式同 lock list）")
    ap.add_argument("--prefetch-experts", type=int, default=0, help="激活感知动态专家预取（1=开启，需 --n-expert）")
    ap.add_argument("--out-dir", default="results")
    ap.add_argument("--port", type=int, default=18140)
    ap.add_argument("--kv-sweep", action="store_true")
    args = ap.parse_args()

    server = find_server_exe()
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    all_rows = []
    for pct in [int(x) for x in args.budgets.split(",")]:
        budget_mb = round(args.model_mb * pct / 100.0, 1)
        print(f"[..] budget {pct}% = {budget_mb} MB ...", flush=True)
        row = run_one(server, args.model, args.model_id, budget_mb,
                      Path(args.lock_list) if args.lock_list else None,
                      args.port + (pct % 10), args.requests, args.max_tokens,
                      args.n_expert, args.prefetch_layers,
                      Path(args.prefetch_list) if args.prefetch_list else "",
                      args.prefetch_experts)
        row["pct"] = pct
        all_rows.append(row)
        print("     ", {k: v for k, v in row.items() if k not in ("ok",)}, flush=True)

    payload = {"model_id": args.model_id, "model_mb": args.model_mb,
               "sweep": all_rows}
    if args.kv_sweep:
        payload["kv_sweep"] = kv_sweep(server, args.model, args.model_id, args.port + 50)

    out_file = out_dir / f"membudget_{args.model_id}.json"
    out_file.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"saved -> {out_file}")


if __name__ == "__main__":
    main()
