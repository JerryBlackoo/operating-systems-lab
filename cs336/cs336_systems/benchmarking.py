"""End-to-end benchmarking script (card notes/cs336-a2/01-benchmarking-script.md).

    python -m cs336_systems.benchmarking --size small --mode full --warmup 5 --steps 10
    python -m cs336_systems.benchmarking --size xl --mode forward --mixed-precision \
        --memory-profile snapshot.pickle --device cuda
"""

from __future__ import annotations

import argparse
import contextlib
import statistics
import timeit

import torch

from cs336_basics.model import TransformerLM
from cs336_basics.optim import AdamW, cross_entropy

# Table 1 of the handout: (d_model, d_ff, num_layers, num_heads)
MODEL_SIZES = {
    "tiny": (64, 128, 2, 2),  # extra config for CPU smoke tests
    "small": (768, 3072, 12, 12),
    "medium": (1024, 4096, 24, 16),
    "large": (1280, 5120, 36, 20),
    "xl": (2560, 10240, 32, 32),
    "10B": (4608, 12288, 50, 36),
}


def _sync(device: str) -> None:
    if device.startswith("cuda"):
        torch.cuda.synchronize()  # CUDA is async: without this we time nothing


def benchmark(
    size: str = "small",
    mode: str = "full",  # forward | forward_backward | full
    warmup: int = 5,
    steps: int = 10,
    batch_size: int = 4,
    context_length: int = 512,
    vocab_size: int = 10_000,
    device: str = "cpu",
    mixed_precision: bool = False,
    memory_profile: str | None = None,
) -> dict:
    d_model, d_ff, num_layers, num_heads = MODEL_SIZES[size]
    model = TransformerLM(
        vocab_size, context_length, num_layers, d_model, num_heads, d_ff, device=device
    )
    optimizer = AdamW(model.parameters(), lr=1e-4)
    inputs = torch.randint(0, vocab_size, (batch_size, context_length), device=device)
    targets = torch.randint(0, vocab_size, (batch_size, context_length), device=device)

    autocast = (
        torch.autocast(device_type="cuda", dtype=torch.bfloat16)
        if mixed_precision and device.startswith("cuda")
        else contextlib.nullcontext()
    )

    def one_step():
        with autocast:
            logits = model(inputs)
            if mode == "forward":
                _sync(device)
                return
            loss = cross_entropy(logits, targets)
        optimizer.zero_grad()
        loss.backward()
        if mode == "full":
            optimizer.step()
        _sync(device)

    for _ in range(warmup):  # warmup: compilation, allocator, autotuning
        one_step()

    if memory_profile and device.startswith("cuda"):
        torch.cuda.memory._record_memory_history(max_entries=1_000_000)

    times = []
    for _ in range(steps):
        start = timeit.default_timer()
        one_step()
        times.append(timeit.default_timer() - start)

    if memory_profile and device.startswith("cuda"):
        torch.cuda.memory._dump_snapshot(memory_profile)
        torch.cuda.memory._record_memory_history(enabled=None)
        print(f"memory snapshot saved to {memory_profile} (view at pytorch.org/memory_viz)")

    result = {
        "size": size,
        "mode": mode,
        "mean_s": statistics.mean(times),
        "std_s": statistics.stdev(times) if len(times) > 1 else 0.0,
        "steps": steps,
    }
    print(
        f"{size:>6} | {mode:>16} | {result['mean_s']*1e3:9.2f} ms "
        f"+- {result['std_s']*1e3:6.2f} ms over {steps} steps"
    )
    return result


def main(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("--size", default="small", choices=list(MODEL_SIZES))
    p.add_argument("--mode", default="full", choices=["forward", "forward_backward", "full"])
    p.add_argument("--warmup", type=int, default=5)
    p.add_argument("--steps", type=int, default=10)
    p.add_argument("--batch-size", type=int, default=4)
    p.add_argument("--context-length", type=int, default=512)
    p.add_argument("--device", default="cpu")
    p.add_argument("--mixed-precision", action="store_true")
    p.add_argument("--memory-profile", default=None)
    args = p.parse_args(argv)
    benchmark(**vars(args))


if __name__ == "__main__":
    main()
