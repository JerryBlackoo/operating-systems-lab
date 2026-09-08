"""Optimizer state sharding, simplified ZeRO stage 1 (card notes/cs336-a2/08).

Each rank's inner optimizer only holds state for ~1/world_size of the
parameters; after each step, every parameter is broadcast from its owner rank.
"""

from __future__ import annotations

from typing import Any, Type

import torch
import torch.distributed as dist
from torch.optim import Optimizer


class ShardedOptimizer(Optimizer):
    def __init__(self, params, optimizer_cls: Type[Optimizer], **kwargs: Any):
        # Init-order trap (card 08): the base constructor calls add_param_group
        # immediately, so every attribute it touches must already exist.
        self.rank = dist.get_rank()
        self.world_size = dist.get_world_size()
        self.optimizer_cls = optimizer_cls
        self.optimizer_kwargs = kwargs
        self.inner: Optimizer | None = None
        self._owner: dict[torch.nn.Parameter, int] = {}
        self._next_owner = 0
        super().__init__(params, defaults=dict(kwargs))

    def add_param_group(self, param_group: dict[str, Any]) -> None:
        super().add_param_group(param_group)  # record the full group

        # Round-robin ownership; this rank's shard goes into the inner optimizer.
        local_params = []
        for p in param_group["params"]:
            owner = self._next_owner % self.world_size
            self._next_owner += 1
            self._owner[p] = owner
            if owner == self.rank:
                local_params.append(p)

        if local_params:
            local_group = {k: v for k, v in param_group.items() if k != "params"}
            local_group["params"] = local_params
            if self.inner is None:
                self.inner = self.optimizer_cls([local_group], **self.optimizer_kwargs)
            else:
                self.inner.add_param_group(local_group)

    def step(self, closure=None, **kwargs):
        loss = None
        if self.inner is not None:
            loss = self.inner.step(closure, **kwargs)
        elif closure is not None:
            loss = closure()
        # Sync: every parameter is broadcast from the rank that updated it.
        for group in self.param_groups:
            for p in group["params"]:
                dist.broadcast(p.data, src=self._owner[p])
        return loss
