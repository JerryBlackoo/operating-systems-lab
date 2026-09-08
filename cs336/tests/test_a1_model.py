"""A1 cards 03-12: model building blocks, verified against PyTorch references."""

import math

import pytest
import torch
import torch.nn.functional as F

from cs336_basics.model import (
    Embedding,
    Linear,
    MultiHeadSelfAttention,
    RMSNorm,
    RotaryPositionalEmbedding,
    SwiGLU,
    TransformerBlock,
    TransformerLM,
    scaled_dot_product_attention,
    softmax,
)

torch.manual_seed(0)


def test_linear_matches_functional():
    layer = Linear(6, 4)
    x = torch.randn(2, 3, 6)
    expected = F.linear(x, layer.weight)  # y = x W^T with W (out, in)
    torch.testing.assert_close(layer(x), expected)
    assert layer(x).shape == (2, 3, 4)
    assert not hasattr(layer, "bias") or layer.bias is None


def test_linear_hand_example():
    layer = Linear(2, 2)
    with torch.no_grad():
        layer.weight.copy_(torch.tensor([[1.0, 2.0], [3.0, 4.0]]))
    out = layer(torch.tensor([1.0, 1.0]))
    torch.testing.assert_close(out, torch.tensor([3.0, 7.0]))


def test_embedding_matches_functional():
    emb = Embedding(10, 8)
    ids = torch.randint(0, 10, (2, 5))
    torch.testing.assert_close(emb(ids), F.embedding(ids, emb.weight))
    assert emb(ids).shape == (2, 5, 8)


def test_rmsnorm_formula_and_hand_example():
    norm = RMSNorm(2, eps=0.0)
    out = norm(torch.tensor([[3.0, 4.0]]))
    rms = math.sqrt((9 + 16) / 2)
    torch.testing.assert_close(out, torch.tensor([[3.0 / rms, 4.0 / rms]]))


def test_rmsnorm_upcasts_to_avoid_overflow():
    norm = RMSNorm(8)
    x = torch.full((2, 8), 30000.0, dtype=torch.float16)  # 30000^2 overflows fp16
    out = norm(x)
    assert out.dtype == torch.float16
    assert torch.isfinite(out).all()
    torch.testing.assert_close(out, torch.ones_like(out), atol=1e-2, rtol=1e-2)


def test_swiglu_matches_reference():
    ffn = SwiGLU(8, 16)
    x = torch.randn(4, 8)
    expected = ffn.w2(F.silu(ffn.w1(x)) * ffn.w3(x))
    torch.testing.assert_close(ffn(x), expected)


def test_swiglu_default_dff_is_multiple_of_64():
    ffn = SwiGLU(512)
    assert ffn.d_ff == 1344  # handout: 8/3*512 rounded to a multiple of 64


def test_rope_identity_at_position_zero():
    rope = RotaryPositionalEmbedding(theta=10000.0, d_k=8, max_seq_len=16)
    x = torch.randn(2, 1, 8)
    out = rope(x, torch.tensor([0]))
    torch.testing.assert_close(out, x)


def test_rope_hand_rotation():
    # d_k=2, position 1: rotation angle = 1/theta^0 = 1 radian.
    rope = RotaryPositionalEmbedding(theta=10000.0, d_k=2, max_seq_len=4)
    x = torch.tensor([[[1.0, 0.0]]])
    out = rope(x, torch.tensor([1]))
    torch.testing.assert_close(
        out, torch.tensor([[[math.cos(1.0), math.sin(1.0)]]]), atol=1e-6, rtol=1e-6
    )


def test_rope_preserves_norm_and_shape():
    rope = RotaryPositionalEmbedding(theta=10000.0, d_k=16, max_seq_len=32)
    x = torch.randn(3, 4, 10, 16)  # extra batch dims
    pos = torch.arange(10)
    out = rope(x, pos)
    assert out.shape == x.shape
    torch.testing.assert_close(
        out.norm(dim=-1), x.norm(dim=-1), atol=1e-5, rtol=1e-5
    )  # rotations preserve norm


def test_softmax_matches_pytorch_and_is_stable():
    x = torch.randn(2, 3, 5)
    for dim in (0, 1, 2, -1):
        torch.testing.assert_close(softmax(x, dim), torch.softmax(x, dim=dim))
    big = torch.tensor([[1000.0, 1000.0]])
    torch.testing.assert_close(softmax(big, -1), torch.tensor([[0.5, 0.5]]))


def test_sdpa_matches_pytorch_3d_and_4d():
    for shape in [(2, 5, 8), (2, 3, 5, 8)]:
        q = torch.randn(*shape)
        k = torch.randn(*shape)
        v = torch.randn(*shape[:-1], 6)
        expected = F.scaled_dot_product_attention(q, k, v)
        torch.testing.assert_close(scaled_dot_product_attention(q, k, v), expected)


def test_sdpa_causal_mask():
    q = torch.randn(2, 5, 8)
    k = torch.randn(2, 5, 8)
    v = torch.randn(2, 5, 6)
    idx = torch.arange(5)
    mask = idx[:, None] >= idx[None, :]
    expected = F.scaled_dot_product_attention(q, k, v, is_causal=True)
    torch.testing.assert_close(scaled_dot_product_attention(q, k, v, mask), expected)


def test_sdpa_masked_positions_get_zero_probability():
    q = torch.randn(1, 2, 4)
    k = torch.randn(1, 3, 4)
    v = torch.eye(3).unsqueeze(0)  # attention weights become directly observable
    mask = torch.tensor([[True, True, False], [True, False, False]])
    out = scaled_dot_product_attention(q, k, v, mask)
    assert out[0, 0, 2] == 0.0  # masked key contributes nothing
    assert out[0, 1, 1] == 0.0 and out[0, 1, 2] == 0.0
    torch.testing.assert_close(out.sum(-1), torch.ones(1, 2))  # rows still sum to 1


def test_mha_is_causal():
    torch.manual_seed(1)
    mha = MultiHeadSelfAttention(d_model=16, num_heads=4)
    x = torch.randn(1, 6, 16)
    out1 = mha(x)
    x2 = x.clone()
    x2[:, 4:, :] = torch.randn(1, 2, 16)  # perturb the future
    out2 = mha(x2)
    torch.testing.assert_close(out1[:, :4], out2[:, :4])  # past outputs unchanged
    assert not torch.allclose(out1[:, 4:], out2[:, 4:])


def test_mha_shape_with_rope_and_batch_dims():
    rope = RotaryPositionalEmbedding(theta=10000.0, d_k=4, max_seq_len=32)
    mha = MultiHeadSelfAttention(d_model=16, num_heads=4, rope=rope)
    x = torch.randn(2, 3, 8, 16)
    assert mha(x).shape == (2, 3, 8, 16)


def test_transformer_block_reduces_to_identity_when_sublayers_are_zero():
    block = TransformerBlock(d_model=16, num_heads=4, d_ff=32)
    with torch.no_grad():
        block.attn.output_proj.weight.zero_()
        block.ffn.w2.weight.zero_()
    x = torch.randn(2, 5, 16)
    torch.testing.assert_close(block(x), x)  # pure residual stream


def test_transformer_lm_shapes_and_param_count():
    V, L, d, h, dff, layers = 100, 32, 64, 4, 128, 2
    lm = TransformerLM(V, L, layers, d, h, dff)
    ids = torch.randint(0, V, (3, 10))
    logits = lm(ids)
    assert logits.shape == (3, 10, V)
    expected_params = (
        V * d  # embedding
        + layers * (2 * d + 4 * d * d + 3 * d * dff)  # 2 norms + qkvo + swiglu
        + d  # final norm
        + d * V  # lm head
    )
    assert sum(p.numel() for p in lm.parameters()) == expected_params


def test_transformer_lm_is_causal():
    torch.manual_seed(2)
    lm = TransformerLM(50, 16, 2, 32, 4, 64)
    ids = torch.randint(0, 50, (1, 8))
    out1 = lm(ids)
    ids2 = ids.clone()
    ids2[0, 6:] = (ids2[0, 6:] + 1) % 50
    out2 = lm(ids2)
    torch.testing.assert_close(out1[:, :6], out2[:, :6])
