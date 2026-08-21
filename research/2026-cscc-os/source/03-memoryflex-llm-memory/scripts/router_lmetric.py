"""阶段4：内存受限边缘集群的 LMETRIC 乘法调度实验

每个实例 = llama-server + MemoryScheduler 内存预算（边缘节点）；
路由器维护指标工厂：P-token（新 prefill token 数，考虑前缀命中）、BS（排队+运行），
score = P-token × BS（LMETRIC）；对比 LB-only 与线性组合；含两阶段热点检测器。

用法:
  python scripts/router_lmetric.py --trace results/trace_chat.jsonl \
      --instances 4 --model models/qwen2.5-1.5b-instruct-q4_k_m.gguf \
      --budget-pct 60 --concurrency 8 --policy lmetric --out results/cluster_lmetric.json
"""

import argparse
import json
import os
import subprocess
import threading
import time
from collections import deque
from pathlib import Path

import requests


def find_server_exe():
    c = Path(__file__).resolve().parent.parent / "tools/llama.cpp/build/bin/Release/llama-server.exe"
    if not c.is_file():
        raise FileNotFoundError("llama-server.exe not found")
    return c


def char_lcp(a, b, cap=4096):
    n = min(len(a), len(b), cap)
    i = 0
    while i < n and a[i] == b[i]:
        i += 1
    return i


def toks(text):
    return text


class EngineInstance(threading.Thread):
    def __init__(self, idx, port, model, budget_mb, gpu_layers, ctx, cache_reuse):
        super().__init__(daemon=True)
        self.idx = idx
        self.port = port
        self.queue = deque()
        self.cv = threading.Condition()
        self.active = 0
        self.registry = []          # [(user, prompt)]
        self.pending_added = 0      # 队列中待处理的新 prefill token 数（LMETRIC P-token 的排队部分）
        self.results = []
        self.failures = 0
        self.dead = False
        self.stop = threading.Event()
        env = dict(os.environ)
        if budget_mb > 0:
            env["LLAMA_MEM_BUDGET_MB"] = str(budget_mb)
        cmd = [str(find_server_exe()), "--model", str(model), "--host", "127.0.0.1",
               "--port", str(port), "--n-gpu-layers", str(gpu_layers),
               "--ctx-size", str(ctx), "--parallel", "1",
               "--cache-reuse", str(cache_reuse)]
        self.proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.STDOUT, env=env)
        base = f"http://127.0.0.1:{port}/"
        t0 = time.time()
        while time.time() - t0 < 180:
            try:
                if requests.get(base + "health", timeout=2).status_code == 200:
                    break
            except Exception:
                pass
            time.sleep(0.3)
        self.base = base

    def bs(self):
        return self.active + len(self.queue)

    def hit_len(self, prompt):
        best = 0
        for _, p in self.registry[-64:]:
            best = max(best, char_lcp(prompt, p))
        return best

    def submit(self, req):
        with self.cv:
            self.queue.append(req)
            self.cv.notify()
        self.pending_added += req.get("added", 0)
        # 提交即登记前缀（实例处理完该请求后其 KV 槽即持有此前缀）
        with threading.Lock():
            self.registry.append((req.get("user"), req["prompt"]))
            if len(self.registry) > 512:
                self.registry = self.registry[-256:]

    def run(self):
        while not self.stop.is_set():
            with self.cv:
                while not self.queue and not self.stop.is_set():
                    self.cv.wait(0.2)
                if not self.queue:
                    continue
                req = self.queue.popleft()
                self.active += 1
            try:
                self._send(req)
            finally:
                self.active -= 1

    def _send(self, req):
        t0 = time.time()
        payload = {"model": "m",
                   "messages": [{"role": "user", "content": req["prompt"]}],
                   "max_tokens": 12, "temperature": 0.0, "stream": True}
        ttft = tpot = None
        prompt_ms = None
        toks = []
        try:
            with requests.post(self.base + "v1/chat/completions", json=payload,
                               stream=True, timeout=120) as r:
                for line in r.iter_lines(decode_unicode=True):
                    s = line if isinstance(line, str) else line.decode("utf-8", "replace")
                    if "content" in s and "delta" in s:
                        toks.append(time.time())
                    if '"timings"' in s and 'prompt_ms' in s:
                        try:
                            import json as _json
                            i = s.find("data:")
                            obj = _json.loads(s[i + 5:].strip())
                            prompt_ms = obj.get("timings", {}).get("prompt_ms")
                        except Exception:
                            pass
            if toks:
                ttft = (toks[0] - t0) * 1000
                tpot = [(b - a) * 1000 for a, b in zip(toks, toks[1:])]
        except Exception:
            self.failures += 1
            if self.proc.poll() is not None:
                self.dead = True
        self.pending_added -= req.get("added", 0)
        self.results.append({
            "req": req.get("id"), "user": req.get("user"), "topic": req.get("topic"),
            "instance": self.idx, "ttft_ms": ttft,
            "tpot_mean_ms": (sum(tpot) / len(tpot)) if tpot else None,
            "n_out_tokens": len(toks),
            "prefill_ms": prompt_ms,
            "hit_len": req.get("hit", 0),
        })
    def close(self):
        self.stop.set()
        self.proc.terminate()
        try:
            self.proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class Router:
    def __init__(self, instances, policy, linear_alpha=0.5, use_detector=False):
        self.instances = instances
        self.policy = policy
        self.alpha = linear_alpha
        self.decisions = []
        self.use_detector = use_detector
        self.window = deque(maxlen=20)
        self.alarms = []
        self.hot_phase = {}         # topic -> {m, count, active}

    def _hotspot(self):
        """两阶段热点检测（论文式2）：x/x̄ > |M|/|M̄| 时告警"""
        if len(self.window) < 20:
            return None
        counts = {}
        insts = {}
        for d in self.window:
            t = d["topic"]
            counts[t] = counts.get(t, 0) + 1
            insts.setdefault(t, set()).add(d["inst"])
        for t, c in counts.items():
            x = c / len(self.window)
            m = len(insts[t])
            n = len(self.instances)
            if x > 0.2 and x / max(1e-6, 1 - x) > m / max(1e-6, n - m):
                return t, insts[t]
        return None

    def pick(self, req, alarm_class=None):
        cands = [i for i in self.instances if not i.dead] or self.instances
        if alarm_class is not None and req.get("topic") == alarm_class:
            cands = [i for i in cands
                     if not any(u == req.get("user") for u, _ in i.registry[-32:])] or self.instances
        hotspot = self._hotspot() if self.use_detector else None
        if hotspot is not None:
            # 第一阶段：检测到热点，进入两阶段判定（仅首次初始化，避免反复清零）
            topic, m_insts = hotspot
            if topic not in self.hot_phase:
                self.hot_phase[topic] = {"m": set(m_insts), "count": 0, "active": False}
        phase = self.hot_phase.get(req.get("topic"))
        if phase is not None and not phase["active"] and req.get("topic") is not None:
            # 第二阶段：连续 2|M| 个请求在热点实例上得分更小时才过滤 M（论文式 2|M|）
            m = phase["m"]
            mbar = [i for i in self.instances if i.idx not in m and not i.dead]
            if m and mbar:
                best_m = min(self._score(i, req)[0] for i in self.instances if i.idx in m)
                best_mbar = min(self._score(i, req)[0] for i in mbar)
                if best_m <= best_mbar:
                    phase["count"] += 1
                else:
                    phase["count"] = 0
                if phase["count"] >= 2 * len(m):
                    phase["active"] = True
                    self.alarms.append({"req": req.get("id"), "topic": req.get("topic"),
                                        "hot_insts": sorted(m),
                                        "action": "2|M| consecutive -> filter M + LB"})
        if phase is not None and phase["active"]:
            cands = [i for i in self.instances if i.idx not in phase["m"] and not i.dead] or cands
        scores = []
        for inst in cands:
            score, hit, added = self._score(inst, req)
            scores.append((score, inst.idx, hit, added))
        scores.sort(key=lambda x: x[0])
        _, idx, hit, added = scores[0]
        inst = self.instances[idx]
        req["hit"] = hit
        req["added"] = added
        self.decisions.append({"req": req.get("id"), "inst": inst.idx,
                               "policy": self.policy, "topic": req.get("topic"),
                               "hit": hit, "added": added})
        if self.use_detector:
            self.window.append(self.decisions[-1])
        return inst

    def _score(self, inst, req):
        hit = inst.hit_len(req["prompt"])
        added = max(1, len(req["prompt"]) // 4 - hit // 4)
        bs = inst.bs()
        if self.policy == "lmetric":
            score = (inst.pending_added + added) * max(1, bs)
        elif self.policy == "lb":
            score = bs
        else:  # linear
            score = self.alpha * added + (1 - self.alpha) * bs
        return score, hit, added


def run_experiment(args, policy, out_file, alarm_class=None, use_detector=False):
    model_mb = args.model_mb
    budget_mb = round(model_mb * args.budget_pct / 100.0, 1)
    instances = [EngineInstance(i, args.port + i, args.model, budget_mb,
                                args.gpu_layers, args.ctx, args.cache_reuse)
                 for i in range(args.instances)]
    for inst in instances:
        inst.start()
    router = Router(instances, policy, args.linear_alpha, use_detector=use_detector)

    reqs = [json.loads(l) for l in Path(args.trace).read_text(encoding="utf-8").splitlines()]
    for i, r in enumerate(reqs):
        r["id"] = i
    # 流式派发：按到达顺序提交，限制在飞请求数（保持前缀可逐步形成）
    for req in reqs:
        while True:
            inflight = sum(i.active + len(i.queue) for i in instances)
            if inflight < args.concurrency:
                break
            time.sleep(0.02)
        inst = router.pick(req, alarm_class)
        inst.submit(req)

    deadline = time.time() + 60
    while time.time() < deadline:
        done = sum(len(i.results) for i in instances)
        if done >= len(reqs):
            break
        time.sleep(0.5)

    results = []
    for inst in instances:
        results += inst.results
    for inst in instances:
        inst.close()
    Path(out_file).with_suffix(".detail.json").write_text(
        json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")

    ttft = [r["ttft_ms"] for r in results if r["ttft_ms"]]
    tpot = [r["tpot_mean_ms"] for r in results if r["tpot_mean_ms"]]
    prefill = [r["prefill_ms"] for r in results if r["prefill_ms"]]
    hits = [r["hit_len"] // 4 for r in results]
    bs_by_inst = {}
    for r in results:
        bs_by_inst.setdefault(r["instance"], []).append(1)
    import statistics
    out = {
        "policy": policy, "n_requests": len(reqs), "n_instances": args.instances,
        "budget_pct": args.budget_pct, "concurrency": args.concurrency,
        "ttft_mean_ms": round(statistics.mean(ttft), 1) if ttft else None,
        "ttft_p50_ms": round(statistics.median(ttft), 1) if ttft else None,
        "ttft_p95_ms": round(sorted(ttft)[int(0.95 * len(ttft))], 1) if ttft else None,
        "tpot_mean_ms": round(statistics.mean(tpot), 1) if tpot else None,
        "prefill_mean_ms": round(statistics.mean(prefill), 1) if prefill else None,
        "prefill_p50_ms": round(statistics.median(prefill), 1) if prefill else None,
        "kv_hit_mean_tokens": round(statistics.mean(hits), 1) if hits else None,
        "kv_hit_rate": round(sum(1 for h in hits if h > 16) / len(hits), 3) if hits else None,
        "load_balance_std": round(statistics.pstdev([len(i.results) for i in instances]), 1),
        "n_alarms": len(router.alarms),
        "n_failures": sum(i.failures for i in instances),
        "decisions": router.decisions[:50],
    }
    Path(out_file).write_text(json.dumps(out, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"[{policy}] TTFT mean={out['ttft_mean_ms']}ms P50={out['ttft_p50_ms']} "
          f"P95={out['ttft_p95_ms']} | TPOT={out['tpot_mean_ms']}ms | "
          f"hit={out['kv_hit_rate']} | load_std={out['load_balance_std']}")
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trace", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--model-mb", type=float, required=True)
    ap.add_argument("--instances", type=int, default=4)
    ap.add_argument("--budget-pct", type=int, default=60)
    ap.add_argument("--gpu-layers", type=int, default=0)
    ap.add_argument("--ctx", type=int, default=2048)
    ap.add_argument("--cache-reuse", type=int, default=32)
    ap.add_argument("--concurrency", type=int, default=8)
    ap.add_argument("--linear-alpha", type=float, default=0.5)
    ap.add_argument("--policies", default="lb,lmetric,linear")
    ap.add_argument("--out-dir", default="results")
    ap.add_argument("--port", type=int, default=18200)
    ap.add_argument("--hotspot", action="store_true", help="运行合成热点+检测器回退实验")
    ap.add_argument("--detector", action="store_true", help="启用两阶段热点检测器")
    args = ap.parse_args()

    Path(args.out_dir).mkdir(parents=True, exist_ok=True)
    out = {}
    for pol in args.policies.split(","):
        out[pol] = run_experiment(args, pol, Path(args.out_dir) / f"cluster_{pol}.json",
                                  use_detector=args.detector)
    if args.hotspot:
        save = Path(args.out_dir)
        args.trace = save / "trace_hotspot.jsonl"
        out["hotspot_lm"] = run_experiment(args, "lmetric", save / "cluster_hotspot_lm.json")
        out["hotspot_lm_det"] = run_experiment(args, "lmetric", save / "cluster_hotspot_lm_det.json",
                                               use_detector=True)
    summary = Path(args.out_dir) / "cluster_summary.json"
    summary.write_text(json.dumps({k: {kk: vv for kk, vv in v.items()
                                       if kk not in ("decisions",)} for k, v in out.items()},
                                  ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"saved -> {summary}")


if __name__ == "__main__":
    main()
