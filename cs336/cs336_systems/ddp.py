"""Distributed data parallel training (cards notes/cs336-a2/05, 06, 07).

Three stages of the handout's DDP progression:
1. naive_ddp_all_reduce_grads: one all-reduce per parameter after backward.
2. flat_ddp_all_reduce_grads: flatten all grads, single all-reduce, unflatten.
3. DDPOverlapIndividualParameters: async all-reduce per parameter as soon as
   its gradient is accumulated (overlaps communication with backward).

All work with gloo on CPU (for tests/dev) and nccl on GPU.
"""

from __future__ import annotations

import torch
import torch.distributed as dist
from torch import nn
from torch._utils import _flatten_dense_tensors, _unflatten_dense_tensors


def broadcast_parameters(model: nn.Module, src: int = 0) -> None:
    """All ranks start from rank `src`'s initialization (card 05, invariant)."""
    for p in model.parameters():
        dist.broadcast(p.data, src=src)


def naive_ddp_all_reduce_grads(model: nn.Module) -> None:
    """One synchronous all-reduce per parameter; average across ranks."""
    world_size = dist.get_world_size()
    for p in model.parameters():
        if p.grad is not None:
            dist.all_reduce(p.grad, op=dist.ReduceOp.SUM)
            p.grad /= world_size


def flat_ddp_all_reduce_grads(model: nn.Module) -> None:
    """Single all-reduce on the flattened concatenation of all gradients."""
    world_size = dist.get_world_size()
    grads = [p.grad for p in model.parameters() if p.grad is not None]
    if not grads:
        return
    flat = _flatten_dense_tensors(grads)
    dist.all_reduce(flat, op=dist.ReduceOp.SUM)
    flat /= world_size
    for grad, synced in zip(grads, _unflatten_dense_tensors(flat, grads)):
        grad.copy_(synced)  # write back (unflatten returns new tensors)


class DDPOverlapIndividualParameters(nn.Module):
    """DDP container that overlaps gradient communication with the backward pass.

    Usage:
        ddp_model = DDPOverlapIndividualParameters(model)
        loss = loss_fn(ddp_model(x), y)
        loss.backward()
        ddp_model.finish_gradient_synchronization()
        optimizer.step()
    """

    def __init__(self, module: nn.Module):
        super().__init__()
        self.module = module
        self.world_size = dist.get_world_size()
        self._handles: list = []
        broadcast_parameters(module)
        for p in module.parameters():
            if p.requires_grad:
                p.register_post_accumulate_grad_hook(self._make_hook())

    def _make_hook(self):
        def hook(param: torch.nn.Parameter) -> None:
            # Average, then kick off the async all-reduce immediately: the rest
            # of the backward pass keeps computing while this communicates.
            param.grad /= self.world_size
            handle = dist.all_reduce(param.grad, op=dist.ReduceOp.SUM, async_op=True)
            self._handles.append(handle)

        return hook

    def forward(self, *inputs, **kwargs):
        return self.module(*inputs, **kwargs)

    def finish_gradient_synchronization(self) -> None:
        for handle in self._handles:
            handle.wait()
        self._handles.clear()
