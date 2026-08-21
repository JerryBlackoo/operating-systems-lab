"""A2 cards: FlashAttention-2 (CPU) + DDP / sharded optimizer / FSDP (gloo, 2 procs).

The distributed tests verify the golden property from the cards: distributed
training must produce bitwise-comparable parameters to single-process
full-batch training.
"""

import math
import os
import socket
import tempfile

import pytest
import torch
import torch.distributed as dist
import torch.multiprocessing as mp
import torch.nn.functional as F
from torch import nn

from cs336_basics.optim import AdamW
from cs336_systems.benchmarking import benchmark
from cs336_systems.flash_attention import (
    FlashAttentionPyTorch,
    flash_attention_backward,
    flash_attention_forward_pytorch,
)

SEED = 1234
WORLD_SIZE = 2
DIM_IN, DIM_HIDDEN, DIM_OUT = 8, 16, 4
N_SAMPLES, N_STEPS, LR = 8, 3, 1e-2


# ============================================================ FlashAttention-2
def _naive_attention(q, k, v, is_causal=False):
    d = q.shape[-1]
    scores = q @ k.transpose(-2, -1) / math.sqrt(d)
    if is_causal:
        n_q, n_k = scores.shape[-2:]
        idx_q = torch.arange(n_q)[:, None]
        idx_k = torch.arange(n_k)[None, :]
        scores = scores.masked_fill(idx_q < idx_k, float("-inf"))
    probs = torch.softmax(scores, dim=-1)
    return probs @ v, torch.logsumexp(
        q @ k.transpose(-2, -1) / math.sqrt(d)
        + (torch.where(idx_q >= idx_k, 0.0, -1e6) if is_causal else 0.0),
        dim=-1,
    )


@pytest.mark.parametrize("is_causal", [False, True])
@pytest.mark.parametrize("n_q,n_k,d", [(32, 32, 16), (64, 32, 16), (48, 80, 32)])
def test_flash_forward_matches_naive(is_causal, n_q, n_k, d):
    torch.manual_seed(0)
    q, k, v = torch.randn(2, n_q, d), torch.randn(2, n_k, d), torch.randn(2, n_k, d)
    o_flash, l_flash = flash_attention_forward_pytorch(q, k, v, is_causal)
    o_ref, l_ref = _naive_attention(q, k, v, is_causal)
    torch.testing.assert_close(o_flash, o_ref, atol=1e-5, rtol=1e-5)
    torch.testing.assert_close(l_flash, l_ref, atol=1e-4, rtol=1e-5)


@pytest.mark.parametrize("is_causal", [False, True])
def test_flash_backward_matches_autograd(is_causal):
    torch.manual_seed(1)
    q = torch.randn(2, 32, 16, requires_grad=True)
    k = torch.randn(2, 32, 16, requires_grad=True)
    v = torch.randn(2, 32, 16, requires_grad=True)
    do = torch.randn(2, 32, 16)

    o_ref, _ = _naive_attention(q, k, v, is_causal)
    dq_ref, dk_ref, dv_ref = torch.autograd.grad(o_ref, (q, k, v), do)

    with torch.no_grad():
        o, l = flash_attention_forward_pytorch(q, k, v, is_causal)
        dq, dk, dv = flash_attention_backward(q, k, v, o, do, l, is_causal)
    torch.testing.assert_close(dq, dq_ref, atol=1e-4, rtol=1e-4)
    torch.testing.assert_close(dk, dk_ref, atol=1e-4, rtol=1e-4)
    torch.testing.assert_close(dv, dv_ref, atol=1e-4, rtol=1e-4)


def test_flash_autograd_function_end_to_end():
    torch.manual_seed(2)
    q1 = torch.randn(1, 32, 16, requires_grad=True)
    k1 = torch.randn(1, 32, 16, requires_grad=True)
    v1 = torch.randn(1, 32, 16, requires_grad=True)
    q2, k2, v2 = (t.detach().clone().requires_grad_(True) for t in (q1, k1, v1))

    out = FlashAttentionPyTorch.apply(q1, k1, v1, True)
    out.sum().backward()
    ref, _ = _naive_attention(q2, k2, v2, True)
    ref.sum().backward()
    torch.testing.assert_close(out, ref, atol=1e-5, rtol=1e-5)
    torch.testing.assert_close(q1.grad, q2.grad, atol=1e-4, rtol=1e-4)
    torch.testing.assert_close(k1.grad, k2.grad, atol=1e-4, rtol=1e-4)
    torch.testing.assert_close(v1.grad, v2.grad, atol=1e-4, rtol=1e-4)


def test_benchmark_smoke():
    result = benchmark(size="tiny", mode="full", warmup=1, steps=2,
                       batch_size=2, context_length=32, vocab_size=64)
    assert result["mean_s"] > 0


# ====================================================== distributed (gloo, CPU)
class TinyNet(nn.Module):
    def __init__(self):
        super().__init__()
        from cs336_basics.model import Linear, RMSNorm

        self.fc1 = Linear(DIM_IN, DIM_HIDDEN)
        self.norm = RMSNorm(DIM_HIDDEN)
        self.fc2 = Linear(DIM_HIDDEN, DIM_OUT)

    def forward(self, x):
        return self.fc2(torch.relu(self.norm(self.fc1(x))))


def _make_data():
    g = torch.Generator().manual_seed(999)
    X = torch.randn(N_SAMPLES, DIM_IN, generator=g)
    Y = torch.randn(N_SAMPLES, DIM_OUT, generator=g)
    return X, Y


def _single_process_reference():
    """Full-batch training baseline every distributed variant must match."""
    torch.manual_seed(SEED)
    model = TinyNet()
    opt = AdamW(model.parameters(), lr=LR, weight_decay=0.01)
    X, Y = _make_data()
    for _ in range(N_STEPS):
        loss = F.mse_loss(model(X), Y)
        opt.zero_grad()
        loss.backward()
        opt.step()
    return {name: p.detach().clone() for name, p in model.named_parameters()}


def _init_pg(rank, world_size, port):
    dist.init_process_group(
        "gloo", init_method=f"tcp://127.0.0.1:{port}", rank=rank, world_size=world_size
    )


def _free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _ddp_worker(rank, world_size, port, variant, out_path):
    from cs336_systems.ddp import (
        DDPOverlapIndividualParameters,
        broadcast_parameters,
        flat_ddp_all_reduce_grads,
        naive_ddp_all_reduce_grads,
    )

    _init_pg(rank, world_size, port)
    torch.manual_seed(SEED)  # same init everywhere; broadcast makes it airtight
    model = TinyNet()
    if variant == "overlap":
        model = DDPOverlapIndividualParameters(model)
    else:
        broadcast_parameters(model)
    opt = AdamW(model.parameters(), lr=LR, weight_decay=0.01)
    X, Y = _make_data()
    shard = slice(rank * (N_SAMPLES // world_size), (rank + 1) * (N_SAMPLES // world_size))
    for _ in range(N_STEPS):
        loss = F.mse_loss(model(X[shard]), Y[shard])
        opt.zero_grad()
        loss.backward()
        if variant == "naive":
            naive_ddp_all_reduce_grads(model)
        elif variant == "flat":
            flat_ddp_all_reduce_grads(model)
        elif variant == "overlap":
            model.finish_gradient_synchronization()
        opt.step()
    if rank == 0:
        inner = model.module if variant == "overlap" else model
        torch.save({n: p.detach() for n, p in inner.named_parameters()}, out_path)
    dist.destroy_process_group()


@pytest.mark.parametrize("variant", ["naive", "flat", "overlap"])
def test_ddp_matches_single_process(variant, tmp_path):
    out_path = str(tmp_path / "params.pt")
    mp.spawn(
        _ddp_worker,
        args=(WORLD_SIZE, _free_port(), variant, out_path),
        nprocs=WORLD_SIZE,
        join=True,
    )
    reference = _single_process_reference()
    trained = torch.load(out_path, weights_only=True)
    for name, ref in reference.items():
        torch.testing.assert_close(trained[name], ref, atol=1e-6, rtol=1e-5)


def _sharded_optimizer_worker(rank, world_size, port, out_path):
    from cs336_systems.ddp import broadcast_parameters, naive_ddp_all_reduce_grads
    from cs336_systems.sharded_optimizer import ShardedOptimizer

    _init_pg(rank, world_size, port)
    torch.manual_seed(SEED)
    model = TinyNet()
    broadcast_parameters(model)
    opt = ShardedOptimizer(model.parameters(), AdamW, lr=LR, weight_decay=0.01)
    X, Y = _make_data()
    shard = slice(rank * (N_SAMPLES // world_size), (rank + 1) * (N_SAMPLES // world_size))
    for _ in range(N_STEPS):
        loss = F.mse_loss(model(X[shard]), Y[shard])
        opt.zero_grad()
        loss.backward()
        naive_ddp_all_reduce_grads(model)  # DDP grads + sharded optimizer states
        opt.step()
    if rank == 0:
        n_local = sum(p.numel() for g in opt.inner.param_groups for p in g["params"])
        n_total = sum(p.numel() for p in model.parameters())
        torch.save(
            {
                "params": {n: p.detach() for n, p in model.named_parameters()},
                "n_local": n_local,
                "n_total": n_total,
            },
            out_path,
        )
    dist.destroy_process_group()


def test_sharded_optimizer_matches_single_process(tmp_path):
    out_path = str(tmp_path / "params.pt")
    mp.spawn(
        _sharded_optimizer_worker,
        args=(WORLD_SIZE, _free_port(), out_path),
        nprocs=WORLD_SIZE,
        join=True,
    )
    reference = _single_process_reference()
    result = torch.load(out_path, weights_only=True)
    for name, ref in reference.items():
        torch.testing.assert_close(result["params"][name], ref, atol=1e-6, rtol=1e-5)
    # Each rank's inner optimizer only owns a fraction of the parameters.
    assert 0 < result["n_local"] < result["n_total"]


def _fsdp_worker(rank, world_size, port, out_path):
    from cs336_systems.fsdp import FSDP

    _init_pg(rank, world_size, port)
    torch.manual_seed(SEED)
    model = TinyNet()
    fsdp = FSDP(model)
    opt = AdamW(list(fsdp.trainable_parameters()), lr=LR, weight_decay=0.01)
    X, Y = _make_data()
    shard = slice(rank * (N_SAMPLES // world_size), (rank + 1) * (N_SAMPLES // world_size))
    for _ in range(N_STEPS):
        loss = F.mse_loss(fsdp(X[shard]), Y[shard])
        opt.zero_grad()
        loss.backward()
        fsdp.finish_gradient_synchronization()
        opt.step()
    # Rebuild full weights from shards to compare against the reference.
    full = {}
    for name, holder in fsdp._sharded.items():
        full[name + ".weight"] = holder.materialize().detach()
    for name, p in model.named_parameters():
        full[name] = p.detach()
    shard_numel = sum(h.shard.numel() for h in fsdp._sharded.values())
    full_numel = sum(h.numel for h in fsdp._sharded.values())
    if rank == 0:
        torch.save(
            {"params": full, "shard_numel": shard_numel, "full_numel": full_numel}, out_path
        )
    dist.destroy_process_group()


def test_fsdp_matches_single_process(tmp_path):
    out_path = str(tmp_path / "params.pt")
    mp.spawn(
        _fsdp_worker,
        args=(WORLD_SIZE, _free_port(), out_path),
        nprocs=WORLD_SIZE,
        join=True,
    )
    reference = _single_process_reference()
    result = torch.load(out_path, weights_only=True)
    for name, ref in reference.items():
        torch.testing.assert_close(result["params"][name], ref, atol=1e-5, rtol=1e-5)
    # Each rank stores only ~1/world_size of the sharded weights.
    assert result["shard_numel"] * WORLD_SIZE <= result["full_numel"] + WORLD_SIZE * 64
    assert result["shard_numel"] < result["full_numel"]
