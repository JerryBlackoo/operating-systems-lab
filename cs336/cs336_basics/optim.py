"""Loss, optimizer, LR schedule, gradient clipping (cards notes/cs336-a1/13..16)."""

from __future__ import annotations

import math
from collections.abc import Callable, Iterable

import torch


def cross_entropy(logits: torch.Tensor, targets: torch.Tensor) -> torch.Tensor:
    """Mean cross-entropy from raw logits (..., vocab) and integer targets (...).

    Numerically stable: subtract max, cancel log/exp (never materialize softmax).
    """
    m = logits.max(dim=-1, keepdim=True).values
    shifted = logits - m
    log_sum_exp = shifted.exp().sum(dim=-1).log()
    target_logit = shifted.gather(-1, targets.unsqueeze(-1)).squeeze(-1)
    return (log_sum_exp - target_logit).mean()


def perplexity(losses: torch.Tensor) -> torch.Tensor:
    return torch.exp(losses.mean())


class AdamW(torch.optim.Optimizer):
    """AdamW per Algorithm 1 of the handout (decoupled weight decay, t starts at 1)."""

    def __init__(self, params, lr=1e-3, betas=(0.9, 0.999), eps=1e-8, weight_decay=0.01):
        if lr < 0:
            raise ValueError(f"Invalid learning rate: {lr}")
        if not 0.0 <= betas[0] < 1.0 or not 0.0 <= betas[1] < 1.0:
            raise ValueError(f"Invalid betas: {betas}")
        defaults = {"lr": lr, "betas": betas, "eps": eps, "weight_decay": weight_decay}
        super().__init__(params, defaults)

    @torch.no_grad()
    def step(self, closure: Callable | None = None):
        loss = None if closure is None else closure()
        for group in self.param_groups:
            lr = group["lr"]
            beta1, beta2 = group["betas"]
            eps = group["eps"]
            weight_decay = group["weight_decay"]
            for p in group["params"]:
                if p.grad is None:
                    continue
                grad = p.grad
                state = self.state[p]
                if not state:
                    state["t"] = 0
                    state["m"] = torch.zeros_like(p)
                    state["v"] = torch.zeros_like(p)
                t = state["t"] + 1
                m, v = state["m"], state["v"]

                # Decoupled weight decay with the raw lr (not alpha_t).
                if weight_decay != 0.0:
                    p.mul_(1.0 - lr * weight_decay)

                m.mul_(beta1).add_(grad, alpha=1.0 - beta1)
                v.mul_(beta2).addcmul_(grad, grad, value=1.0 - beta2)
                alpha_t = lr * math.sqrt(1.0 - beta2**t) / (1.0 - beta1**t)
                p.addcdiv_(m, v.sqrt().add_(eps), value=-alpha_t)

                state["t"] = t
        return loss


def lr_cosine_schedule(
    t: int, alpha_max: float, alpha_min: float, warmup_iters: int, cosine_cycle_iters: int
) -> float:
    """Linear warmup then cosine annealing to alpha_min (card 15)."""
    if t < warmup_iters:
        return t / warmup_iters * alpha_max
    if t <= cosine_cycle_iters:
        progress = (t - warmup_iters) / (cosine_cycle_iters - warmup_iters)
        return alpha_min + 0.5 * (1.0 + math.cos(progress * math.pi)) * (alpha_max - alpha_min)
    return alpha_min


def gradient_clipping(
    parameters: Iterable[torch.nn.Parameter], max_l2_norm: float, eps: float = 1e-6
) -> float:
    """Clip the combined L2 norm of all gradients in place. Returns the pre-clip norm."""
    grads = [p.grad for p in parameters if p.grad is not None]
    if not grads:
        return 0.0
    total_norm = torch.sqrt(sum(g.pow(2).sum() for g in grads))
    if total_norm >= max_l2_norm:
        scale = max_l2_norm / (total_norm + eps)
        for g in grads:
            g.mul_(scale)
    return float(total_norm)
