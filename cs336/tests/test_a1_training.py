"""A1 cards 13-20: loss, optimizer, schedule, clipping, data, checkpoint, train, decode."""

import io
import math

import numpy as np
import pytest
import torch
import torch.nn.functional as F

from cs336_basics.checkpoint import load_checkpoint, save_checkpoint
from cs336_basics.data import get_batch
from cs336_basics.decode import generate, top_p_filter
from cs336_basics.model import TransformerLM
from cs336_basics.optim import (
    AdamW,
    cross_entropy,
    gradient_clipping,
    lr_cosine_schedule,
)

torch.manual_seed(0)


# ---------------------------------------------------------------- cross entropy
def test_cross_entropy_matches_pytorch():
    logits = torch.randn(4, 7, 11)
    targets = torch.randint(0, 11, (4, 7))
    expected = F.cross_entropy(logits.reshape(-1, 11), targets.reshape(-1))
    torch.testing.assert_close(cross_entropy(logits, targets), expected)


def test_cross_entropy_is_stable_for_large_logits():
    logits = torch.tensor([[1000.0, 0.0]])
    targets = torch.tensor([0])
    loss = cross_entropy(logits, targets)
    assert torch.isfinite(loss)
    torch.testing.assert_close(loss, torch.tensor(0.0), atol=1e-6, rtol=0)


def test_cross_entropy_hand_example():
    # o=[0,0], target 0 -> loss = log 2
    loss = cross_entropy(torch.zeros(1, 2), torch.tensor([0]))
    torch.testing.assert_close(loss, torch.tensor(math.log(2.0)))


# ---------------------------------------------------------------------- AdamW
def test_adamw_matches_pytorch_adamw():
    torch.manual_seed(3)
    p_ours = torch.nn.Parameter(torch.randn(10, 10))
    p_ref = torch.nn.Parameter(p_ours.detach().clone())
    ours = AdamW([p_ours], lr=1e-2, betas=(0.9, 0.999), eps=1e-8, weight_decay=0.01)
    ref = torch.optim.AdamW([p_ref], lr=1e-2, betas=(0.9, 0.999), eps=1e-8, weight_decay=0.01)
    for _ in range(5):
        grad = torch.randn(10, 10)
        p_ours.grad = grad.clone()
        p_ref.grad = grad.clone()
        ours.step()
        ref.step()
    # eps placement differs microscopically between the two formulations.
    torch.testing.assert_close(p_ours, p_ref, atol=1e-5, rtol=1e-5)


def test_adamw_state_starts_at_t_equals_one():
    p = torch.nn.Parameter(torch.ones(1))
    opt = AdamW([p], lr=1.0, betas=(0.9, 0.999), weight_decay=0.0)
    p.grad = torch.ones(1)
    opt.step()
    # Hand-check first step: m=0.1, v=0.001, alpha_1 = sqrt(1-b2)/(1-b1)
    # update = alpha_1 * m / (sqrt(v)+eps) ~= 1.0
    torch.testing.assert_close(p.data, torch.tensor([0.0]), atol=1e-3, rtol=0)


# ------------------------------------------------------------------- schedule
def test_lr_cosine_schedule_boundary_values():
    a_max, a_min, tw, tc = 1.0, 0.1, 10, 110
    assert lr_cosine_schedule(0, a_max, a_min, tw, tc) == 0.0
    assert lr_cosine_schedule(tw, a_max, a_min, tw, tc) == pytest.approx(a_max)
    mid = (tw + tc) // 2
    assert lr_cosine_schedule(mid, a_max, a_min, tw, tc) == pytest.approx((a_max + a_min) / 2)
    assert lr_cosine_schedule(tc, a_max, a_min, tw, tc) == pytest.approx(a_min)
    assert lr_cosine_schedule(tc + 100, a_max, a_min, tw, tc) == a_min
    # warmup is linear
    assert lr_cosine_schedule(5, a_max, a_min, tw, tc) == pytest.approx(0.5)


# ------------------------------------------------------------------- clipping
def test_gradient_clipping_matches_pytorch():
    params_ours = [torch.nn.Parameter(torch.randn(4, 4)) for _ in range(3)]
    params_ref = [torch.nn.Parameter(p.detach().clone()) for p in params_ours]
    for p_o, p_r in zip(params_ours, params_ref):
        g = torch.randn(4, 4) * 10
        p_o.grad = g.clone()
        p_r.grad = g.clone()
    gradient_clipping(params_ours, 1.0)
    torch.nn.utils.clip_grad_norm_(params_ref, 1.0)
    for p_o, p_r in zip(params_ours, params_ref):
        torch.testing.assert_close(p_o.grad, p_r.grad)


def test_gradient_clipping_hand_example_and_noop():
    p1 = torch.nn.Parameter(torch.zeros(1))
    p2 = torch.nn.Parameter(torch.zeros(1))
    p1.grad = torch.tensor([3.0])
    p2.grad = torch.tensor([4.0])
    gradient_clipping([p1, p2], 1.0)  # total norm 5 -> scale by ~1/5
    torch.testing.assert_close(p1.grad, torch.tensor([0.6]), atol=1e-5, rtol=1e-5)
    torch.testing.assert_close(p2.grad, torch.tensor([0.8]), atol=1e-5, rtol=1e-5)
    p1.grad = torch.tensor([0.1])
    p2.grad = torch.tensor([0.1])
    gradient_clipping([p1, p2], 1.0)  # below max -> untouched
    torch.testing.assert_close(p1.grad, torch.tensor([0.1]))


# ----------------------------------------------------------------- data loading
def test_get_batch_shapes_alignment_and_dtype():
    np.random.seed(0)
    data = np.arange(1000, dtype=np.uint16)  # value == position makes alignment checkable
    inputs, targets = get_batch(data, batch_size=8, context_length=16, device="cpu")
    assert inputs.shape == targets.shape == (8, 16)
    assert inputs.dtype == targets.dtype == torch.int64
    torch.testing.assert_close(targets, inputs + 1)  # next-token shift
    assert int(targets.max()) < 1000


# ---------------------------------------------------------------- checkpointing
def _tiny_lm():
    return TransformerLM(vocab_size=32, context_length=16, num_layers=1, d_model=16, num_heads=2, d_ff=32)


def test_checkpoint_roundtrip_and_resume_equivalence():
    torch.manual_seed(4)
    np.random.seed(4)
    data = np.random.randint(0, 32, size=2000, dtype=np.uint16)

    def step(model, opt):
        x, y = get_batch(data, 4, 16, "cpu")
        loss = cross_entropy(model(x), y)
        opt.zero_grad()
        loss.backward()
        opt.step()

    model = _tiny_lm()
    opt = AdamW(model.parameters(), lr=1e-3)
    for _ in range(3):
        step(model, opt)
    buf = io.BytesIO()
    save_checkpoint(model, opt, 3, buf)
    buf.seek(0)

    model2 = _tiny_lm()
    opt2 = AdamW(model2.parameters(), lr=1e-3)
    it = load_checkpoint(buf, model2, opt2)
    assert it == 3
    for p1, p2 in zip(model.parameters(), model2.parameters()):
        torch.testing.assert_close(p1, p2)

    # Continued training must stay in lockstep (optimizer state incl. m/v/t restored).
    np.random.seed(7)
    torch.manual_seed(7)
    step(model, opt)
    np.random.seed(7)
    torch.manual_seed(7)
    step(model2, opt2)
    for p1, p2 in zip(model.parameters(), model2.parameters()):
        torch.testing.assert_close(p1, p2)


# --------------------------------------------------------- end-to-end overfit
def test_overfit_single_batch():
    """Card 19's sanity check: a correct implementation drives loss to ~0 on one batch."""
    torch.manual_seed(5)
    model = TransformerLM(vocab_size=16, context_length=32, num_layers=2, d_model=32, num_heads=4, d_ff=64)
    opt = AdamW(model.parameters(), lr=3e-3, weight_decay=0.0)
    pattern = torch.arange(33).remainder(16).unsqueeze(0).repeat(4, 1)
    inputs, targets = pattern[:, :-1], pattern[:, 1:]
    first_loss = None
    for it in range(150):
        loss = cross_entropy(model(inputs), targets)
        if first_loss is None:
            first_loss = loss.item()
        opt.zero_grad()
        loss.backward()
        opt.step()
    final_loss = loss.item()
    assert final_loss < 0.1, f"failed to memorize one batch: {first_loss:.3f} -> {final_loss:.3f}"


# --------------------------------------------------------------------- decode
def test_top_p_filter_hand_example():
    probs = torch.tensor([[0.5, 0.3, 0.15, 0.05]])
    out = top_p_filter(probs, 0.7)
    torch.testing.assert_close(out, torch.tensor([[0.625, 0.375, 0.0, 0.0]]))


def test_top_p_keeps_at_least_one_token():
    probs = torch.tensor([[0.9, 0.1]])
    out = top_p_filter(probs, 0.05)  # first token already exceeds p
    torch.testing.assert_close(out, torch.tensor([[1.0, 0.0]]))


def test_top_p_one_is_identity():
    probs = torch.softmax(torch.randn(2, 10), dim=-1)
    torch.testing.assert_close(top_p_filter(probs, 1.0), probs)


def test_generate_greedy_is_deterministic_and_stops_at_eos():
    torch.manual_seed(6)
    model = _tiny_lm()
    prompt = [1, 2, 3]
    out1 = generate(model, prompt, max_new_tokens=10, temperature=0.0)
    out2 = generate(model, prompt, max_new_tokens=10, temperature=0.0)
    assert out1 == out2 and len(out1) == 10
    eos = out1[0]
    out3 = generate(model, prompt, max_new_tokens=10, temperature=0.0, eos_token_id=eos)
    assert out3 == [eos]  # stops immediately after emitting eos


def test_generate_respects_context_length():
    model = _tiny_lm()  # context_length 16
    prompt = list(range(1, 15))
    out = generate(model, prompt, max_new_tokens=20, temperature=0.0)
    assert len(out) == 20  # sliding window keeps generation going past the context limit
