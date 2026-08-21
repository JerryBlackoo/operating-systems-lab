"""运行时内存/IO 时间线（赛题任务1：参数加载的访存行为）

对比 mmap（按需缺页加载）与 --no-mmap（启动即全量读入）两种模式下，
生成一个长请求，采样 进程 RSS / 缺页数 / 磁盘读取字节 随时间变化。

用法: python scripts/mem_timeline.py --model models/xx.gguf --out-dir docs/figures
"""

import argparse
import json
import subprocess
import sys
import threading
import time
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import psutil
import requests


def find_server_exe():
    c = Path(__file__).resolve().parent.parent / "tools/llama.cpp/build/bin/Release/llama-server.exe"
    if not c.is_file():
        raise FileNotFoundError("llama-server.exe not found")
    return c


def sample(proc, stop, out, interval=0.05):
    t0 = time.time()
    while not stop.is_set():
        try:
            m = proc.memory_info()
            io = proc.io_counters()
            pf = getattr(m, "pfaults", getattr(m, "num_page_faults", None))
            out.append({"t": round(time.time() - t0, 3),
                        "rss_mb": round(m.rss / 1e6, 2),
                        "vms_mb": round(m.vms / 1e6, 2),
                        "pfaults": pf,
                        "io_read_mb": round(io.read_bytes / 1e6, 2)})
        except Exception:
            pass
        stop.wait(interval)


def run_once(server, model, use_mmap, port, max_tokens=192):
    cmd = [str(server), "--model", str(model), "--host", "127.0.0.1", "--port", str(port),
           "--n-gpu-layers", "0", "--ctx-size", "2048", "--parallel", "1"]
    if not use_mmap:
        cmd.append("--no-mmap")
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    ps = psutil.Process(proc.pid)
    samples = []
    stop = threading.Event()
    th = threading.Thread(target=sample, args=(ps, stop, samples), daemon=True)
    th.start()
    try:
        base = f"http://127.0.0.1:{port}/"
        deadline = time.time() + 120
        while time.time() < deadline:
            try:
                if requests.get(base + "health", timeout=2).status_code == 200:
                    break
            except Exception:
                pass
            time.sleep(0.3)
        prompt = ("请以较长的篇幅详细阐述操作系统虚拟内存的基本原理，"
                  "包括分页、缺页中断、页面置换算法等，尽量写到三百字以上。")
        payload = {"model": "m", "messages": [{"role": "user", "content": prompt}],
                   "max_tokens": max_tokens, "temperature": 0.0, "stream": True}
        r = requests.post(base + "v1/chat/completions", json=payload, stream=True, timeout=300)
        for line in r.iter_lines(decode_unicode=True):
            if line and "DONE" in line:
                break
    finally:
        time.sleep(0.5)
        stop.set()
        th.join(timeout=5)
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()
    return samples


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--out-dir", default="docs/figures")
    args = ap.parse_args()

    server = find_server_exe()
    model = Path(args.model)
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    results = {}
    for use_mmap, label in [(True, "mmap (on-demand)"), (False, "no-mmap (eager)")]:
        port = 18090 + (0 if use_mmap else 1)
        print(f"[..] running {label} ...")
        samples = run_once(server, model, use_mmap, port)
        results[label] = samples
        (out_dir / f"mem_timeline_{'mmap' if use_mmap else 'nommap'}.json").write_text(
            json.dumps(samples, indent=2), encoding="utf-8")

    fig, axes = plt.subplots(1, 2, figsize=(13, 4.2))
    for label, s in results.items():
        if not s:
            continue
        t = [x["t"] for x in s]
        axes[0].plot(t, [x["rss_mb"] for x in s], label=label)
        axes[1].plot(t, [x["io_read_mb"] for x in s], label=label)
    axes[0].set_xlabel("time (s)"); axes[0].set_ylabel("RSS (MB)")
    axes[0].set_title("resident memory over time"); axes[0].legend()
    axes[1].set_xlabel("time (s)"); axes[1].set_ylabel("disk read (MB)")
    axes[1].set_title("disk IO over time"); axes[1].legend()
    fig.tight_layout()
    fig.savefig(out_dir / "mem_timeline.png", dpi=120)
    plt.close(fig)

    for label, s in results.items():
        if s:
            peak = max(x["rss_mb"] for x in s)
            io = max(x["io_read_mb"] for x in s)
            print(f"{label}: peak RSS={peak} MB, total disk read={io} MB, samples={len(s)}")
    print(f"saved -> {out_dir}/mem_timeline*.json / .png")


if __name__ == "__main__":
    main()
