"""Decoding: temperature scaling + top-p sampling (card notes/cs336-a1/20-decoding.md)."""

from __future__ import annotations

import torch

from .model import softmax


def top_p_filter(probs: torch.Tensor, p: float) -> torch.Tensor:
    """Zero out everything outside the smallest prefix with cumulative prob >= p, renormalize.

    Always keeps at least the highest-probability token.
    """
    if p >= 1.0:
        return probs
    sorted_probs, sorted_idx = torch.sort(probs, descending=True, dim=-1)
    cumulative = torch.cumsum(sorted_probs, dim=-1)
    # Keep positions whose cumulative mass *before* them is < p (minimal prefix reaching >= p).
    keep_sorted = (cumulative - sorted_probs) < p
    keep_sorted[..., 0] = True
    keep = torch.zeros_like(keep_sorted).scatter(-1, sorted_idx, keep_sorted)
    filtered = probs * keep
    return filtered / filtered.sum(dim=-1, keepdim=True)


@torch.no_grad()
def generate(
    model: torch.nn.Module,
    prompt_ids: list[int] | torch.Tensor,
    max_new_tokens: int,
    temperature: float = 1.0,
    top_p: float = 1.0,
    eos_token_id: int | None = None,
    context_length: int | None = None,
    device: str = "cpu",
) -> list[int]:
    """Autoregressively sample until eos or max_new_tokens. Returns only new token ids."""
    model.eval()
    if context_length is None:
        context_length = getattr(model, "context_length", None)
    ids = torch.as_tensor(list(prompt_ids), dtype=torch.long, device=device).unsqueeze(0)
    generated: list[int] = []
    for _ in range(max_new_tokens):
        window = ids[:, -context_length:] if context_length else ids
        logits = model(window)[:, -1, :]  # only the last position matters
        if temperature <= 1e-6:
            next_id = logits.argmax(dim=-1, keepdim=True)
        else:
            probs = softmax(logits / temperature, dim=-1)
            probs = top_p_filter(probs, top_p)
            next_id = torch.multinomial(probs, num_samples=1)
        ids = torch.cat([ids, next_id], dim=1)
        token = int(next_id.item())
        generated.append(token)
        if eos_token_id is not None and token == eos_token_id:
            break
    return generated
