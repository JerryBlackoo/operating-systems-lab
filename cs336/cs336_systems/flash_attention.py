"""FlashAttention-2 (cards notes/cs336-a2/02, 03).

- flash_attention_forward_pytorch: tiled online-softmax forward (Algorithm 1),
  pure PyTorch, runs anywhere. Returns (O, L) where L is the row logsumexp.
- flash_attention_backward: recomputation-based backward using L and
  D = rowsum(O * dO); plain PyTorch, wrappable in torch.compile.
- FlashAttentionPyTorch: torch.autograd.Function tying the two together.
- flash_fwd_kernel / FlashAttentionTriton: the fused Triton kernel (CUDA only).
"""

from __future__ import annotations

import math

import torch

NEG_INF_APPROX = -1e6  # handout: add -1e6 to masked scores (not -inf)


# ------------------------------------------------------------ tiled forward
def flash_attention_forward_pytorch(
    Q: torch.Tensor,
    K: torch.Tensor,
    V: torch.Tensor,
    is_causal: bool = False,
    q_tile_size: int = 16,
    k_tile_size: int = 16,
) -> tuple[torch.Tensor, torch.Tensor]:
    """FlashAttention-2 forward, tiled with running (m, l, O). Never materializes
    the full attention matrix. Q: (B, Nq, d), K/V: (B, Nk, d)."""
    B, n_q, d = Q.shape
    n_k = K.shape[1]
    scale = 1.0 / math.sqrt(d)

    O = torch.empty(B, n_q, d, dtype=Q.dtype, device=Q.device)
    L = torch.empty(B, n_q, dtype=torch.float32, device=Q.device)

    q32, k32, v32 = Q.float(), K.float(), V.float()

    for q_start in range(0, n_q, q_tile_size):
        q_end = min(q_start + q_tile_size, n_q)
        q_tile = q32[:, q_start:q_end]  # (B, Bq, d)
        bq = q_end - q_start

        o_tile = torch.zeros(B, bq, d, device=Q.device)
        l_run = torch.zeros(B, bq, device=Q.device)
        m_run = torch.full((B, bq), float("-inf"), device=Q.device)

        for k_start in range(0, n_k, k_tile_size):
            k_end = min(k_start + k_tile_size, n_k)
            k_tile = k32[:, k_start:k_end]
            v_tile = v32[:, k_start:k_end]

            scores = torch.einsum("bqd,bkd->bqk", q_tile, k_tile) * scale
            if is_causal:
                q_idx = torch.arange(q_start, q_end, device=Q.device)[:, None]
                k_idx = torch.arange(k_start, k_end, device=Q.device)[None, :]
                scores = scores + torch.where(q_idx >= k_idx, 0.0, NEG_INF_APPROX)

            m_new = torch.maximum(m_run, scores.max(dim=-1).values)
            p_tilde = torch.exp(scores - m_new.unsqueeze(-1))
            correction = torch.exp(m_run - m_new)
            l_run = correction * l_run + p_tilde.sum(dim=-1)
            o_tile = correction.unsqueeze(-1) * o_tile + torch.einsum(
                "bqk,bkd->bqd", p_tilde, v_tile
            )
            m_run = m_new

        O[:, q_start:q_end] = (o_tile / l_run.unsqueeze(-1)).to(Q.dtype)
        L[:, q_start:q_end] = m_run + torch.log(l_run)

    return O, L


# ------------------------------------------------------- recomputed backward
def flash_attention_backward(
    Q: torch.Tensor,
    K: torch.Tensor,
    V: torch.Tensor,
    O: torch.Tensor,
    dO: torch.Tensor,
    L: torch.Tensor,
    is_causal: bool = False,
) -> tuple[torch.Tensor, torch.Tensor, torch.Tensor]:
    """Backward from the handout: recompute P = exp(S - L) (no softmax needed),
    with D = rowsum(O * dO)."""
    d = Q.shape[-1]
    scale = 1.0 / math.sqrt(d)
    q32, k32, v32 = Q.float(), K.float(), V.float()
    o32, do32 = O.float(), dO.float()

    D = (o32 * do32).sum(dim=-1)  # (B, Nq)
    S = torch.einsum("bqd,bkd->bqk", q32, k32) * scale
    if is_causal:
        n_q, n_k = S.shape[-2], S.shape[-1]
        q_idx = torch.arange(n_q, device=Q.device)[:, None]
        k_idx = torch.arange(n_k, device=Q.device)[None, :]
        S = S + torch.where(q_idx >= k_idx, 0.0, NEG_INF_APPROX)  # same mask as forward

    P = torch.exp(S - L.unsqueeze(-1))  # already normalized thanks to logsumexp
    dV = torch.einsum("bqk,bqd->bkd", P, do32)
    dP = torch.einsum("bqd,bkd->bqk", do32, v32)
    dS = P * (dP - D.unsqueeze(-1))
    dQ = torch.einsum("bqk,bkd->bqd", dS, k32) * scale
    dK = torch.einsum("bqk,bqd->bkd", dS, q32) * scale
    return dQ.to(Q.dtype), dK.to(K.dtype), dV.to(V.dtype)


class FlashAttentionPyTorch(torch.autograd.Function):
    """autograd.Function: tiled forward + recomputation backward."""

    @staticmethod
    def forward(ctx, Q, K, V, is_causal=False):
        O, L = flash_attention_forward_pytorch(Q, K, V, is_causal)
        ctx.save_for_backward(L, Q, K, V, O)
        ctx.is_causal = is_causal
        return O

    @staticmethod
    def backward(ctx, dO):
        L, Q, K, V, O = ctx.saved_tensors
        dQ, dK, dV = flash_attention_backward(Q, K, V, O, dO, L, ctx.is_causal)
        return dQ, dK, dV, None  # None for is_causal


# ----------------------------------------------------------- Triton kernel
try:  # pragma: no cover - requires CUDA GPU
    import triton
    import triton.language as tl

    @triton.jit
    def flash_fwd_kernel(
        Q_ptr, K_ptr, V_ptr,
        O_ptr, L_ptr,
        stride_qb, stride_qq, stride_qd,
        stride_kb, stride_kk, stride_kd,
        stride_vb, stride_vk, stride_vd,
        stride_ob, stride_oq, stride_od,
        stride_lb, stride_lq,
        N_QUERIES, N_KEYS,
        scale,
        D: tl.constexpr,
        Q_TILE_SIZE: tl.constexpr,
        K_TILE_SIZE: tl.constexpr,
        is_causal: tl.constexpr,
    ):
        query_tile_index = tl.program_id(0)
        batch_index = tl.program_id(1)

        Q_block_ptr = tl.make_block_ptr(
            Q_ptr + batch_index * stride_qb,
            shape=(N_QUERIES, D), strides=(stride_qq, stride_qd),
            offsets=(query_tile_index * Q_TILE_SIZE, 0),
            block_shape=(Q_TILE_SIZE, D), order=(1, 0),
        )
        K_block_ptr = tl.make_block_ptr(
            K_ptr + batch_index * stride_kb,
            shape=(N_KEYS, D), strides=(stride_kk, stride_kd),
            offsets=(0, 0), block_shape=(K_TILE_SIZE, D), order=(1, 0),
        )
        V_block_ptr = tl.make_block_ptr(
            V_ptr + batch_index * stride_vb,
            shape=(N_KEYS, D), strides=(stride_vk, stride_vd),
            offsets=(0, 0), block_shape=(K_TILE_SIZE, D), order=(1, 0),
        )
        O_block_ptr = tl.make_block_ptr(
            O_ptr + batch_index * stride_ob,
            shape=(N_QUERIES, D), strides=(stride_oq, stride_od),
            offsets=(query_tile_index * Q_TILE_SIZE, 0),
            block_shape=(Q_TILE_SIZE, D), order=(1, 0),
        )
        L_block_ptr = tl.make_block_ptr(
            L_ptr + batch_index * stride_lb,
            shape=(N_QUERIES,), strides=(stride_lq,),
            offsets=(query_tile_index * Q_TILE_SIZE,),
            block_shape=(Q_TILE_SIZE,), order=(0,),
        )

        q = tl.load(Q_block_ptr, boundary_check=(0, 1), padding_option="zero")
        # On-chip accumulators in float32.
        o = tl.zeros((Q_TILE_SIZE, D), dtype=tl.float32)
        l = tl.zeros((Q_TILE_SIZE,), dtype=tl.float32)
        m = tl.full((Q_TILE_SIZE,), value=float("-inf"), dtype=tl.float32)

        q_idx = query_tile_index * Q_TILE_SIZE + tl.arange(0, Q_TILE_SIZE)
        for k_start in range(0, N_KEYS, K_TILE_SIZE):
            k = tl.load(K_block_ptr, boundary_check=(0, 1), padding_option="zero")
            v = tl.load(V_block_ptr, boundary_check=(0, 1), padding_option="zero")

            s = tl.dot(q, tl.trans(k)) * scale  # (Q_TILE, K_TILE)
            if is_causal:
                k_idx = k_start + tl.arange(0, K_TILE_SIZE)
                s = s + tl.where(q_idx[:, None] >= k_idx[None, :], 0.0, -1e6)

            m_new = tl.maximum(m, tl.max(s, axis=1))
            p = tl.exp(s - m_new[:, None])
            correction = tl.exp(m - m_new)
            l = correction * l + tl.sum(p, axis=1)
            o = correction[:, None] * o
            o = tl.dot(p.to(v.dtype), v, acc=o)
            m = m_new

            K_block_ptr = K_block_ptr.advance((K_TILE_SIZE, 0))
            V_block_ptr = V_block_ptr.advance((K_TILE_SIZE, 0))

        o = o / l[:, None]
        tl.store(O_block_ptr, o.to(O_block_ptr.type.element_ty), boundary_check=(0, 1))
        tl.store(L_block_ptr, m + tl.log(l), boundary_check=(0,))

    class FlashAttentionTriton(torch.autograd.Function):
        Q_TILE_SIZE = 16
        K_TILE_SIZE = 16

        @staticmethod
        def forward(ctx, Q, K, V, is_causal=False):
            assert Q.is_cuda, "Triton kernel requires CUDA tensors"
            B, n_q, d = Q.shape
            n_k = K.shape[1]
            O = torch.empty_like(Q)
            L = torch.empty(B, n_q, dtype=torch.float32, device=Q.device)
            grid = (triton.cdiv(n_q, FlashAttentionTriton.Q_TILE_SIZE), B)
            flash_fwd_kernel[grid](
                Q, K, V, O, L,
                Q.stride(0), Q.stride(1), Q.stride(2),
                K.stride(0), K.stride(1), K.stride(2),
                V.stride(0), V.stride(1), V.stride(2),
                O.stride(0), O.stride(1), O.stride(2),
                L.stride(0), L.stride(1),
                n_q, n_k, 1.0 / math.sqrt(d),
                D=d,
                Q_TILE_SIZE=FlashAttentionTriton.Q_TILE_SIZE,
                K_TILE_SIZE=FlashAttentionTriton.K_TILE_SIZE,
                is_causal=is_causal,
            )
            ctx.save_for_backward(L, Q, K, V, O)
            ctx.is_causal = is_causal
            return O

        @staticmethod
        def backward(ctx, dO):
            L, Q, K, V, O = ctx.saved_tensors
            dQ, dK, dV = flash_attention_backward(Q, K, V, O, dO, L, ctx.is_causal)
            return dQ, dK, dV, None

except ImportError:  # pragma: no cover
    flash_fwd_kernel = None
    FlashAttentionTriton = None
