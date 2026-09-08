"""All-reduce benchmark (card notes/cs336-a2/04-distributed-communication.md).

    python -m cs336_systems.distributed_benchmark --backend gloo --world-size 2 \
        --sizes-mb 1 10
"""

from __future__ import annotations

import argparse
import os
import timeit

import torch
import torch.distributed as dist
import torch.multiprocessing as mp


def setup(rank: int, world_size: int, backend: str, port: str = "29500") -> None:
    os.environ["MASTER_ADDR"] = "localhost"
    os.environ["MASTER_PORT"] = port
    dist.init_process_group(backend, rank=rank, world_size=world_size)
    if backend == "nccl":
        torch.cuda.set_device(rank)


def _worker(rank: int, world_size: int, backend: str, sizes_mb: list[float], warmup: int, iters: int):
    setup(rank, world_size, backend)
    device = f"cuda:{rank}" if backend == "nccl" else "cpu"
    for size_mb in sizes_mb:
        numel = int(size_mb * 1024 * 1024 / 4)  # float32 = 4 bytes
        data = torch.randn(numel, device=device)
        for _ in range(warmup):
            dist.all_reduce(data, async_op=False)
            if backend == "nccl":
                torch.cuda.synchronize()
        times = []
        for _ in range(iters):
            start = timeit.default_timer()
            dist.all_reduce(data, async_op=False)
            if backend == "nccl":
                torch.cuda.synchronize()
            times.append(timeit.default_timer() - start)
        # Aggregate timings across ranks (they vary slightly).
        gathered: list = [None] * world_size
        dist.all_gather_object(gathered, times)
        if rank == 0:
            flat = [t for ts in gathered for t in ts]
            mean_ms = sum(flat) / len(flat) * 1e3
            print(f"{backend} | world={world_size} | {size_mb:8.1f} MB | {mean_ms:8.3f} ms")
    dist.destroy_process_group()


def main(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("--backend", default="gloo", choices=["gloo", "nccl"])
    p.add_argument("--world-size", type=int, default=2)
    p.add_argument("--sizes-mb", type=float, nargs="+", default=[1, 10, 100, 1024])
    p.add_argument("--warmup", type=int, default=5)
    p.add_argument("--iters", type=int, default=20)
    args = p.parse_args(argv)
    mp.spawn(
        _worker,
        args=(args.world_size, args.backend, args.sizes_mb, args.warmup, args.iters),
        nprocs=args.world_size,
        join=True,
    )


if __name__ == "__main__":
    main()
