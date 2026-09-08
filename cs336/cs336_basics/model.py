"""Transformer LM building blocks (cards notes/cs336-a1/03..12).

No torch.nn definitions are used except nn.Parameter, container classes,
and torch.nn.init.trunc_normal_ (per the assignment rules).
"""

from __future__ import annotations

import math

import torch
from einops import einsum, rearrange
from torch import nn


class Linear(nn.Module):
    """y = W x, no bias. W stored as (out_features, in_features)."""

    def __init__(self, in_features: int, out_features: int, device=None, dtype=None):
        super().__init__()
        self.in_features = in_features
        self.out_features = out_features
        self.weight = nn.Parameter(
            torch.empty(out_features, in_features, device=device, dtype=dtype)
        )
        std = math.sqrt(2.0 / (in_features + out_features))
        nn.init.trunc_normal_(self.weight, mean=0.0, std=std, a=-3 * std, b=3 * std)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return einsum(x, self.weight, "... d_in, d_out d_in -> ... d_out")


class Embedding(nn.Module):
    """Token id -> vector lookup. Weight is (num_embeddings, embedding_dim)."""

    def __init__(self, num_embeddings: int, embedding_dim: int, device=None, dtype=None):
        super().__init__()
        self.num_embeddings = num_embeddings
        self.embedding_dim = embedding_dim
        self.weight = nn.Parameter(
            torch.empty(num_embeddings, embedding_dim, device=device, dtype=dtype)
        )
        nn.init.trunc_normal_(self.weight, mean=0.0, std=1.0, a=-3.0, b=3.0)

    def forward(self, token_ids: torch.Tensor) -> torch.Tensor:
        return self.weight[token_ids]


class RMSNorm(nn.Module):
    """Root mean square layer norm; upcasts to float32 internally."""

    def __init__(self, d_model: int, eps: float = 1e-5, device=None, dtype=None):
        super().__init__()
        self.d_model = d_model
        self.eps = eps
        self.weight = nn.Parameter(torch.ones(d_model, device=device, dtype=dtype))

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        in_dtype = x.dtype
        x = x.to(torch.float32)
        rms = torch.sqrt(x.pow(2).mean(dim=-1, keepdim=True) + self.eps)
        result = x / rms * self.weight.to(torch.float32)
        return result.to(in_dtype)


def silu(x: torch.Tensor) -> torch.Tensor:
    return x * torch.sigmoid(x)


class SwiGLU(nn.Module):
    """FFN(x) = W2(SiLU(W1 x) * W3 x)."""

    def __init__(self, d_model: int, d_ff: int | None = None, device=None, dtype=None):
        super().__init__()
        if d_ff is None:
            d_ff = round(8 * d_model / 3 / 64) * 64  # ~8/3 d_model, multiple of 64
            d_ff = max(d_ff, 64)
        self.d_model = d_model
        self.d_ff = d_ff
        self.w1 = Linear(d_model, d_ff, device=device, dtype=dtype)
        self.w2 = Linear(d_ff, d_model, device=device, dtype=dtype)
        self.w3 = Linear(d_model, d_ff, device=device, dtype=dtype)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.w2(silu(self.w1(x)) * self.w3(x))


class RotaryPositionalEmbedding(nn.Module):
    """RoPE on adjacent element pairs (2k-1, 2k). No learnable parameters."""

    def __init__(self, theta: float, d_k: int, max_seq_len: int, device=None):
        super().__init__()
        if d_k % 2 != 0:
            raise ValueError("d_k must be even for RoPE")
        inv_freq = theta ** (
            -torch.arange(0, d_k, 2, device=device, dtype=torch.float32) / d_k
        )
        positions = torch.arange(max_seq_len, device=device, dtype=torch.float32)
        angles = torch.outer(positions, inv_freq)  # (max_seq_len, d_k/2)
        self.register_buffer("cos_cached", angles.cos(), persistent=False)
        self.register_buffer("sin_cached", angles.sin(), persistent=False)

    def forward(self, x: torch.Tensor, token_positions: torch.Tensor) -> torch.Tensor:
        # x: (..., seq_len, d_k); token_positions: (..., seq_len)
        cos = self.cos_cached[token_positions]  # (..., seq_len, d_k/2)
        sin = self.sin_cached[token_positions]
        x32 = x.to(torch.float32)
        x_even = x32[..., 0::2]
        x_odd = x32[..., 1::2]
        out_even = x_even * cos - x_odd * sin
        out_odd = x_even * sin + x_odd * cos
        out = torch.stack((out_even, out_odd), dim=-1).flatten(-2)
        return out.to(x.dtype)


def softmax(x: torch.Tensor, dim: int) -> torch.Tensor:
    """Numerically stable softmax along `dim` (subtract the max)."""
    m = x.max(dim=dim, keepdim=True).values
    e = torch.exp(x - m)
    return e / e.sum(dim=dim, keepdim=True)


def scaled_dot_product_attention(
    Q: torch.Tensor,
    K: torch.Tensor,
    V: torch.Tensor,
    mask: torch.Tensor | None = None,
) -> torch.Tensor:
    """softmax(QK^T / sqrt(d_k)) V with optional boolean mask (True = attend)."""
    d_k = Q.shape[-1]
    scores = einsum(Q, K, "... n d_k, ... m d_k -> ... n m") / math.sqrt(d_k)
    if mask is not None:
        scores = scores.masked_fill(~mask, float("-inf"))
    attn = softmax(scores, dim=-1)
    return einsum(attn, V, "... n m, ... m d_v -> ... n d_v")


class MultiHeadSelfAttention(nn.Module):
    """Causal multi-head self-attention with optional shared RoPE module."""

    def __init__(
        self,
        d_model: int,
        num_heads: int,
        rope: RotaryPositionalEmbedding | None = None,
        device=None,
        dtype=None,
    ):
        super().__init__()
        if d_model % num_heads != 0:
            raise ValueError("d_model must be divisible by num_heads")
        self.d_model = d_model
        self.num_heads = num_heads
        self.d_k = d_model // num_heads
        self.rope = rope
        self.q_proj = Linear(d_model, d_model, device=device, dtype=dtype)
        self.k_proj = Linear(d_model, d_model, device=device, dtype=dtype)
        self.v_proj = Linear(d_model, d_model, device=device, dtype=dtype)
        self.output_proj = Linear(d_model, d_model, device=device, dtype=dtype)

    def forward(
        self, x: torch.Tensor, token_positions: torch.Tensor | None = None
    ) -> torch.Tensor:
        seq_len = x.shape[-2]
        q = rearrange(self.q_proj(x), "... s (h d) -> ... h s d", h=self.num_heads)
        k = rearrange(self.k_proj(x), "... s (h d) -> ... h s d", h=self.num_heads)
        v = rearrange(self.v_proj(x), "... s (h d) -> ... h s d", h=self.num_heads)

        if self.rope is not None:
            if token_positions is None:
                token_positions = torch.arange(seq_len, device=x.device)
            # Head dim acts as a batch dim: same rotation for every head.
            q = self.rope(q, token_positions)
            k = self.rope(k, token_positions)

        # Causal mask: True on and below the diagonal (j <= i).
        idx = torch.arange(seq_len, device=x.device)
        mask = idx[:, None] >= idx[None, :]

        out = scaled_dot_product_attention(q, k, v, mask)
        out = rearrange(out, "... h s d -> ... s (h d)")
        return self.output_proj(out)


class TransformerBlock(nn.Module):
    """Pre-norm block: x + MHA(RMSNorm(x)); then z + FFN(RMSNorm(z))."""

    def __init__(
        self,
        d_model: int,
        num_heads: int,
        d_ff: int,
        rope: RotaryPositionalEmbedding | None = None,
        device=None,
        dtype=None,
    ):
        super().__init__()
        self.ln1 = RMSNorm(d_model, device=device, dtype=dtype)
        self.attn = MultiHeadSelfAttention(
            d_model, num_heads, rope=rope, device=device, dtype=dtype
        )
        self.ln2 = RMSNorm(d_model, device=device, dtype=dtype)
        self.ffn = SwiGLU(d_model, d_ff, device=device, dtype=dtype)

    def forward(
        self, x: torch.Tensor, token_positions: torch.Tensor | None = None
    ) -> torch.Tensor:
        z = x + self.attn(self.ln1(x), token_positions)
        return z + self.ffn(self.ln2(z))


class TransformerLM(nn.Module):
    """Embedding -> num_layers pre-norm blocks -> RMSNorm -> LM head (logits)."""

    def __init__(
        self,
        vocab_size: int,
        context_length: int,
        num_layers: int,
        d_model: int,
        num_heads: int,
        d_ff: int,
        rope_theta: float = 10000.0,
        device=None,
        dtype=None,
    ):
        super().__init__()
        self.vocab_size = vocab_size
        self.context_length = context_length
        self.token_embeddings = Embedding(vocab_size, d_model, device=device, dtype=dtype)
        # One RoPE module shared by all layers (buffer, not a parameter).
        self.rope = RotaryPositionalEmbedding(
            theta=rope_theta,
            d_k=d_model // num_heads,
            max_seq_len=context_length,
            device=device,
        )
        self.layers = nn.ModuleList(
            TransformerBlock(d_model, num_heads, d_ff, rope=self.rope, device=device, dtype=dtype)
            for _ in range(num_layers)
        )
        self.ln_final = RMSNorm(d_model, device=device, dtype=dtype)
        self.lm_head = Linear(d_model, vocab_size, device=device, dtype=dtype)

    def forward(self, token_ids: torch.Tensor) -> torch.Tensor:
        x = self.token_embeddings(token_ids)
        for block in self.layers:
            x = block(x)
        x = self.ln_final(x)
        return self.lm_head(x)  # logits: softmax is left to the loss / decoder
