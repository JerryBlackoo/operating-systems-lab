"""A5 cards 02-09: GRPO components, tested on CPU with a tiny model."""

import math
from types import SimpleNamespace

import pytest
import torch

from cs336_alignment.grader import (
    extract_gsm8k_answer,
    question_only_reward_fn,
    r1_zero_reward_fn,
)
from cs336_alignment.grpo import (
    aggregate_loss_across_microbatch,
    compute_group_normalized_rewards,
    compute_policy_gradient_loss,
    compute_rollout_rewards,
    get_response_log_probs,
    grpo_train_step,
    tokenize_prompt_and_output,
)
from cs336_basics.decode import generate
from cs336_basics.model import TransformerLM
from cs336_basics.optim import AdamW

torch.manual_seed(0)

VOCAB = 16


class IntTokenizer:
    """Whitespace-separated integer 'words'; token id == the integer itself."""

    pad_token_id = 0
    eos_token_id = 0

    def __call__(self, text, add_special_tokens=False):
        ids = [int(tok) for tok in text.split()] if text.strip() else []
        return {"input_ids": ids}

    def decode(self, ids):
        return " ".join(str(int(i)) for i in ids)


class HFStyleLM(torch.nn.Module):
    """Wrap our TransformerLM to quack like a HuggingFace model (.logits)."""

    def __init__(self, lm):
        super().__init__()
        self.lm = lm

    def forward(self, input_ids):
        return SimpleNamespace(logits=self.lm(input_ids))


def tiny_model(seed=0):
    torch.manual_seed(seed)
    return HFStyleLM(
        TransformerLM(vocab_size=VOCAB, context_length=32, num_layers=1, d_model=16,
                      num_heads=2, d_ff=32)
    )


# ------------------------------------------------------------------ grader
def test_gsm8k_answer_extraction_and_grading():
    assert extract_gsm8k_answer("Natalia sold ... \n#### 72") == "72"
    r = r1_zero_reward_fn("stuff </think> <answer> 72 </answer>", "72")
    assert r == {"reward": 1.0, "format_reward": 1.0, "answer_reward": 1.0}
    r = r1_zero_reward_fn("stuff </think> <answer> 71 </answer>", "72")
    assert r["reward"] == 0.0 and r["format_reward"] == 1.0
    r = r1_zero_reward_fn("no tags at all 72", "72")
    assert r["format_reward"] == 0.0 and r["reward"] == 0.0
    r = question_only_reward_fn("the answer is \\boxed{1,000}", "1000")
    assert r["reward"] == 1.0


# ---------------------------------------------------------------- card 02
def test_tokenize_prompt_and_output_hand_example():
    tok = IntTokenizer()
    out = tokenize_prompt_and_output(["1 2"], ["3 4"], tok)
    # seq = [1,2,3,4]: input_ids drop last, labels drop first, mask aligned to labels.
    assert out["input_ids"].tolist() == [[1, 2, 3]]
    assert out["labels"].tolist() == [[2, 3, 4]]
    assert out["response_mask"].tolist() == [[False, True, True]]


def test_tokenize_prompt_and_output_padding():
    tok = IntTokenizer()
    out = tokenize_prompt_and_output(["1 2", "5"], ["3 4", "6"], tok)
    assert out["input_ids"].shape == (2, 3)
    # Row 2: seq [5,6] padded to len 4 -> input [5,6,0], labels [6,0,0], mask [T,F,F]
    assert out["input_ids"][1].tolist() == [5, 6, 0]
    assert out["labels"][1].tolist() == [6, 0, 0]
    assert out["response_mask"][1].tolist() == [True, False, False]
    # mask counts exactly the response tokens
    assert int(out["response_mask"].sum()) == 2 + 1


# ---------------------------------------------------------------- card 03
def test_get_response_log_probs_uniform_model():
    class UniformLM(torch.nn.Module):
        def forward(self, input_ids):
            b, s = input_ids.shape
            return SimpleNamespace(logits=torch.zeros(b, s, VOCAB))

    input_ids = torch.tensor([[1, 2, 3]])
    labels = torch.tensor([[2, 3, 4]])
    out = get_response_log_probs(UniformLM(), input_ids, labels, return_token_entropy=True)
    torch.testing.assert_close(
        out["log_probs"], torch.full((1, 3), -math.log(VOCAB))
    )
    torch.testing.assert_close(
        out["token_entropy"], torch.full((1, 3), math.log(VOCAB))
    )


# ---------------------------------------------------------------- card 04
def test_compute_rollout_rewards():
    def reward_fn(resp, gt):
        ok = gt in resp
        return {"reward": float(ok), "format_reward": 1.0, "answer_reward": float(ok)}

    rewards, meta = compute_rollout_rewards(reward_fn, ["a 7 b", "c d", "7"], ["7", "7", "7"])
    assert rewards.tolist() == [1.0, 0.0, 1.0]
    assert meta["mean_reward"] == pytest.approx(2 / 3)
    assert meta["mean_format_reward"] == 1.0


# ---------------------------------------------------------------- card 05
def test_group_normalization_grpo_hand_example():
    raw = torch.tensor([1.0, 0.0, 0.0, 1.0])
    adv, _ = compute_group_normalized_rewards(raw, group_size=4, baseline="mean",
                                              advantage_normalizer="std")
    # mean 0.5; torch.std (Bessel) = sqrt(4*0.25/3) = 0.57735
    expected = torch.tensor([0.5, -0.5, -0.5, 0.5]) / (math.sqrt(1 / 3) + 1e-6)
    torch.testing.assert_close(adv, expected)


def test_group_normalization_drgrpo_and_maxrl():
    raw = torch.tensor([1.0, 0.0, 0.0, 1.0])
    adv, _ = compute_group_normalized_rewards(raw, 4, baseline="mean", advantage_normalizer="none")
    torch.testing.assert_close(adv, torch.tensor([0.5, -0.5, -0.5, 0.5]))
    adv, _ = compute_group_normalized_rewards(raw, 4, baseline="mean", advantage_normalizer="mean")
    torch.testing.assert_close(adv, torch.tensor([1.0, -1.0, -1.0, 1.0]),
                               atol=1e-4, rtol=1e-4)  # (r-0.5)/0.5
    adv, _ = compute_group_normalized_rewards(raw, 4, baseline="none", advantage_normalizer="none")
    torch.testing.assert_close(adv, raw)


def test_group_normalization_all_same_reward_is_safe():
    raw = torch.ones(8)
    adv, _ = compute_group_normalized_rewards(raw, 4)
    assert torch.isfinite(adv).all()
    torch.testing.assert_close(adv, torch.zeros(8))  # std=0 -> eps saves us; A=0


def test_group_normalization_groups_are_independent():
    raw = torch.tensor([1.0, 0.0, 1.0, 1.0])  # two groups of 2
    adv, _ = compute_group_normalized_rewards(raw, 2, baseline="mean", advantage_normalizer="none")
    torch.testing.assert_close(adv, torch.tensor([0.5, -0.5, 0.0, 0.0]))


# ---------------------------------------------------------------- card 06
def test_policy_gradient_loss_on_policy():
    adv = torch.tensor([2.0, -1.0])
    logp = torch.randn(2, 3)
    loss, _ = compute_policy_gradient_loss(adv, logp, "none")
    torch.testing.assert_close(loss, -adv.view(-1, 1) * logp)


def test_noclip_with_fresh_policy_matches_on_policy_gradient():
    adv = torch.tensor([1.5])
    logp = torch.zeros(1, 4, requires_grad=True)
    old = torch.zeros(1, 4)  # same policy -> w=1
    loss_off, _ = compute_policy_gradient_loss(adv, logp, "noclip", old_log_probs=old)
    loss_off.sum().backward()
    grad_off = logp.grad.clone()

    logp2 = torch.zeros(1, 4, requires_grad=True)
    loss_on, _ = compute_policy_gradient_loss(adv, logp2, "none")
    loss_on.sum().backward()
    torch.testing.assert_close(grad_off, logp2.grad)


def test_grpo_clipping_zeroes_gradient_when_out_of_range():
    eps = 0.2
    # positive advantage, weight above 1+eps -> clipped branch, zero gradient
    logp = torch.tensor([[0.5]], requires_grad=True)  # w = e^0.5 ~ 1.65 > 1.2
    old = torch.zeros(1, 1)
    loss, meta = compute_policy_gradient_loss(
        torch.tensor([1.0]), logp, "grpo", old_log_probs=old, cliprange=eps
    )
    loss.sum().backward()
    torch.testing.assert_close(logp.grad, torch.zeros_like(logp))
    assert float(meta["clip_fraction"]) == 1.0
    # in-range weight -> gradient flows
    logp2 = torch.tensor([[0.1]], requires_grad=True)  # w ~ 1.105 < 1.2
    loss2, meta2 = compute_policy_gradient_loss(
        torch.tensor([1.0]), logp2, "grpo", old_log_probs=torch.zeros(1, 1), cliprange=eps
    )
    loss2.sum().backward()
    assert logp2.grad.abs().sum() > 0
    assert float(meta2["clip_fraction"]) == 0.0


def test_gspo_uses_sequence_level_geometric_mean():
    eps = 10.0  # effectively no clipping; isolate the reweighting math
    logp = torch.tensor([[0.2, 0.4, 0.0]], requires_grad=True)
    old = torch.zeros(1, 3)
    mask = torch.tensor([[True, True, False]])  # geo-mean over first two tokens only
    adv = torch.tensor([1.0])
    loss, _ = compute_policy_gradient_loss(
        adv, logp, "gspo", old_log_probs=old, cliprange=eps, response_mask=mask
    )
    s = math.exp((0.2 + 0.4) / 2)
    torch.testing.assert_close(loss, torch.full((1, 3), -s), atol=1e-6, rtol=1e-6)


# ---------------------------------------------------------------- card 07
def test_aggregate_loss_hand_examples():
    per_token = torch.tensor([[1.0, 1.0], [2.0, 0.0]])
    mask = torch.tensor([[True, True], [True, False]])
    seq = aggregate_loss_across_microbatch(per_token, mask, "sequence")
    torch.testing.assert_close(seq, torch.tensor(1.5))  # mean(mean(1,1), 2/1) = mean(1,2)
    const = aggregate_loss_across_microbatch(per_token, mask, "constant", normalization_constant=4)
    torch.testing.assert_close(const, torch.tensor(1.0))  # (1+1+2)/4


# ---------------------------------------------------------------- card 08
def _make_rollout_batch(n_prompts=4, group_size=4):
    """Prompts '1 2', responses of 2 tokens; reward 1 iff response contains '7'."""
    torch.manual_seed(42)
    prompts, responses, gts = [], [], []
    for i in range(n_prompts):
        for j in range(group_size):
            prompts.append("1 2")
            # deterministic mix of rewarded / unrewarded responses
            responses.append("7 3" if (i + j) % 2 == 0 else "4 3")
            gts.append("7")
    return prompts, responses, gts


def _reward_fn(resp, gt):
    ok = gt in resp.split()
    return {"reward": float(ok), "format_reward": 1.0, "answer_reward": float(ok)}


def test_grpo_train_step_updates_parameters_and_logs():
    model = tiny_model()
    opt = AdamW(model.parameters(), lr=1e-3, weight_decay=0.0)
    prompts, responses, gts = _make_rollout_batch()
    before = [p.detach().clone() for p in model.parameters()]
    loss, meta = grpo_train_step(
        model, IntTokenizer(), opt,
        gradient_accumulation_steps=2, max_grad_norm=1.0,
        reward_fn=_reward_fn, repeated_prompts=prompts,
        rollout_responses=responses, repeated_ground_truths=gts, group_size=4,
    )
    assert torch.isfinite(loss)
    for key in ("loss", "grad_norm", "token_entropy", "mean_reward"):
        assert key in meta
    assert meta["mean_reward"] == pytest.approx(0.5)
    changed = any(
        not torch.allclose(b, p.detach()) for b, p in zip(before, model.parameters())
    )
    assert changed


def test_grpo_train_step_gradient_accumulation_equivalence():
    prompts, responses, gts = _make_rollout_batch()
    results = []
    for accum in (1, 4):
        model = tiny_model(seed=123)
        opt = AdamW(model.parameters(), lr=1e-3, weight_decay=0.0)
        grpo_train_step(
            model, IntTokenizer(), opt,
            gradient_accumulation_steps=accum, max_grad_norm=None,
            reward_fn=_reward_fn, repeated_prompts=prompts,
            rollout_responses=responses, repeated_ground_truths=gts, group_size=4,
            prune_zero_advantage=False,
        )
        results.append([p.detach().clone() for p in model.parameters()])
    for p1, p2 in zip(*results):
        torch.testing.assert_close(p1, p2, atol=1e-6, rtol=1e-5)


def test_grpo_train_step_pruning_is_gradient_equivalent():
    prompts, responses, gts = _make_rollout_batch()
    results = []
    for prune in (False, True):
        model = tiny_model(seed=7)
        opt = AdamW(model.parameters(), lr=1e-3, weight_decay=0.0)
        grpo_train_step(
            model, IntTokenizer(), opt,
            gradient_accumulation_steps=2, max_grad_norm=None,
            reward_fn=_reward_fn, repeated_prompts=prompts,
            rollout_responses=responses, repeated_ground_truths=gts, group_size=4,
            baseline="none", advantage_normalizer="none",
            loss_normalization="constant", normalization_constant=64,
            prune_zero_advantage=prune,
        )
        results.append([p.detach().clone() for p in model.parameters()])
    for p1, p2 in zip(*results):
        torch.testing.assert_close(p1, p2, atol=1e-6, rtol=1e-5)


def test_grpo_variants_run():
    prompts, responses, gts = _make_rollout_batch()
    variants = [
        dict(baseline="mean", advantage_normalizer="std", loss_normalization="sequence"),
        dict(baseline="mean", advantage_normalizer="std", loss_normalization="constant",
             normalization_constant=64),
        dict(baseline="mean", advantage_normalizer="none", loss_normalization="constant",
             normalization_constant=64),  # Dr. GRPO
        dict(baseline="none", advantage_normalizer="none", loss_normalization="constant",
             normalization_constant=64),  # RFT
        dict(baseline="mean", advantage_normalizer="mean", loss_normalization="constant",
             normalization_constant=64),  # MaxRL
    ]
    for kwargs in variants:
        model = tiny_model()
        opt = AdamW(model.parameters(), lr=1e-3, weight_decay=0.0)
        loss, _ = grpo_train_step(
            model, IntTokenizer(), opt, gradient_accumulation_steps=2, max_grad_norm=1.0,
            reward_fn=_reward_fn, repeated_prompts=prompts, rollout_responses=responses,
            repeated_ground_truths=gts, group_size=4, **kwargs,
        )
        assert torch.isfinite(loss)


def test_grpo_off_policy_step_runs_with_old_log_probs():
    model = tiny_model()
    opt = AdamW(model.parameters(), lr=1e-3, weight_decay=0.0)
    prompts, responses, gts = _make_rollout_batch()
    tok = IntTokenizer()
    batch = tokenize_prompt_and_output(prompts, responses, tok)
    with torch.no_grad():
        old_log_probs = get_response_log_probs(model, batch["input_ids"], batch["labels"])[
            "log_probs"
        ]
    for method, cliprange in (("noclip", None), ("grpo", 0.2), ("gspo", 3e-4)):
        loss, meta = grpo_train_step(
            model, tok, opt, gradient_accumulation_steps=2, max_grad_norm=1.0,
            reward_fn=_reward_fn, repeated_prompts=prompts, rollout_responses=responses,
            repeated_ground_truths=gts, group_size=4,
            importance_reweighting_method=method, old_log_probs=old_log_probs,
            cliprange=cliprange,
        )
        assert torch.isfinite(loss)
        if method in ("grpo", "gspo"):
            assert "clip_fraction" in meta


# --------------------------------------------------- end-to-end tiny RL loop
def test_grpo_improves_reward_on_toy_task():
    """Full RL loop on CPU: rollouts sampled from the model itself, reward for
    emitting token '7'. Mean reward must improve over training (card 09's
    'validation rewards improving' smoke check, miniaturized)."""
    torch.manual_seed(0)
    model = tiny_model(seed=0)
    opt = AdamW(model.parameters(), lr=5e-3, weight_decay=0.0)
    tok = IntTokenizer()
    n_prompts, group_size, gen_len = 4, 8, 3
    prompt_ids = [1, 2]

    def sample_batch():
        prompts, responses, gts = [], [], []
        for _ in range(n_prompts):
            for _ in range(group_size):
                new_ids = generate(model.lm, prompt_ids, max_new_tokens=gen_len,
                                   temperature=1.0)
                prompts.append("1 2")
                responses.append(tok.decode(new_ids))
                gts.append("7")
        return prompts, responses, gts

    def mean_reward(responses, gts):
        return sum(_reward_fn(r, g)["reward"] for r, g in zip(responses, gts)) / len(responses)

    initial_rewards = []
    for step in range(30):
        prompts, responses, gts = sample_batch()
        if step < 3:
            initial_rewards.append(mean_reward(responses, gts))
        grpo_train_step(
            model, tok, opt, gradient_accumulation_steps=1, max_grad_norm=1.0,
            reward_fn=_reward_fn, repeated_prompts=prompts, rollout_responses=responses,
            repeated_ground_truths=gts, group_size=group_size,
        )
    final_rewards = [mean_reward(*sample_batch()[1:]) for _ in range(3)]
    initial, final = sum(initial_rewards) / 3, sum(final_rewards) / 3
    assert final > initial + 0.2, f"RL did not improve reward: {initial:.2f} -> {final:.2f}"
