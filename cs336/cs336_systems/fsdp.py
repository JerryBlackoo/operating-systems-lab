"""Minimal fully-sharded data parallel (card notes/cs336-a2/09).

Each rank permanently stores only a 1/world_size shard of every Linear /
Embedding weight. Before each forward, full weights are rebuilt with an
autograd-aware all-gather (torch.distributed.nn.functional.all_gather), so
the backward pass automatically reduces gradients back onto the local shard.
Small layers (norms etc.) are left unsharded and gradient-synced like DDP.

Simplifications vs. a production FSDP (noted on the card): no two-layers-ahead
prefetch, no explicit reduce-scatter (the autograd all-gather's backward
performs the equivalent reduction), full weights live for the duration of the
forward+backward graph. Correctness (training parity with DDP) is what the
tests verify.
"""

from __future__ import annotations

import torch
import torch.distributed as dist
import torch.distributed.nn.functional as dist_nn
from torch import nn

from .ddp import broadcast_parameters


def _pad_to_divisible(flat: torch.Tensor, world_size: int) -> torch.Tensor:
    remainder = flat.numel() % world_size
    if remainder:
        flat = torch.cat([flat, flat.new_zeros(world_size - remainder)])
    return flat


class _ShardedWeight(nn.Module):
    """Owns the local shard; materializes the full weight via autograd all-gather."""

    def __init__(self, full_weight: torch.Tensor, world_size: int, rank: int):
        super().__init__()
        self.shape = full_weight.shape
        self.numel = full_weight.numel()
        flat = _pad_to_divisible(full_weight.detach().reshape(-1), world_size)
        shard_size = flat.numel() // world_size
        self.shard = nn.Parameter(
            flat[rank * shard_size : (rank + 1) * shard_size].clone()
        )

    def materialize(self) -> torch.Tensor:
        gathered = dist_nn.all_gather(self.shard)  # autograd-aware: backward reduces
        full = torch.cat(gathered)[: self.numel]
        return full.reshape(self.shape)


class FSDP(nn.Module):
    """Wrap a module; shard every Linear/Embedding `weight` across ranks."""

    SHARDED_TYPES = ("Linear", "Embedding")

    def __init__(self, module: nn.Module, compute_dtype: torch.dtype | None = None):
        super().__init__()
        if dist.get_world_size() > 1:
            broadcast_parameters(module)  # identical weights before sharding
        self.module = module
        self.compute_dtype = compute_dtype
        self.world_size = dist.get_world_size()
        self.rank = dist.get_rank()
        self._sharded: dict[str, _ShardedWeight] = {}
        self._shard_holder = nn.ModuleDict()

        for name, submodule in module.named_modules():
            if type(submodule).__name__ in self.SHARDED_TYPES and hasattr(
                submodule, "weight"
            ):
                weight = submodule.weight
                holder = _ShardedWeight(weight, self.world_size, self.rank)
                key = name.replace(".", "/") or "root"
                self._sharded[name] = holder
                self._shard_holder[key] = holder
                # Replace the Parameter with a plain (empty) tensor attribute;
                # the real value is installed at forward time. (Careful NOT to
                # attach the holder to the submodule: nn.Module attribute
                # assignment would register it as a child and its shard would
                # leak into module.parameters().)
                del submodule.weight
                submodule.weight = torch.empty(0)

        # Unsharded leftover parameters (norm gains, etc.) sync like DDP.
        self._unsharded = [p for p in module.parameters() if p.requires_grad]
        self._handles: list = []
        for p in self._unsharded:
            p.register_post_accumulate_grad_hook(self._make_unsharded_hook())

    def _make_unsharded_hook(self):
        def hook(param):
            param.grad /= self.world_size
            self._handles.append(
                dist.all_reduce(param.grad, op=dist.ReduceOp.SUM, async_op=True)
            )

        return hook

    def _install_full_weights(self) -> None:
        for name, holder in self._sharded.items():
            submodule = self.module.get_submodule(name)
            full = holder.materialize()
            if self.compute_dtype is not None:
                full = full.to(self.compute_dtype)
            submodule.weight = full  # plain tensor in the autograd graph

    def _drop_full_weights(self) -> None:
        for name in self._sharded:
            submodule = self.module.get_submodule(name)
            submodule.weight = torch.empty(0)  # free the module's reference

    def forward(self, *inputs, **kwargs):
        self._install_full_weights()
        try:
            return self.module(*inputs, **kwargs)
        finally:
            # The autograd graph keeps what backward needs; the module itself
            # no longer references the gathered full weights.
            self._drop_full_weights()

    def finish_gradient_synchronization(self) -> None:
        for handle in self._handles:
            handle.wait()
        self._handles.clear()
        # The all-gather backward SUMS shard gradients across ranks; divide to
        # average (matching DDP's mean-gradient convention).
        for holder in self._sharded.values():
            if holder.shard.grad is not None:
                holder.shard.grad /= self.world_size

    def trainable_parameters(self):
        """Parameters an optimizer should own on this rank: local shards +
        (replicated) unsharded parameters."""
        for holder in self._sharded.values():
            yield holder.shard
        yield from self._unsharded
