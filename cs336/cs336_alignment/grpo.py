"""GRPO components (cards notes/cs336-a5/02..08).

All functions support the full range of variants from the handout:
  baseline:                        "mean" | "none"
  advantage_normalizer:            "std" (GRPO) | "none" (Dr.GRPO) | "mean" (MaxRL)
  importance_reweighting_method:   "none" | "noclip" | "grpo" (PPO clip) | "gspo"
  loss_normalization:              "sequence" | "constant"

Models only need HF-style duck typing: model(input_ids).logits.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Literal

import torch
import torch.nn.functional as F


# --------------------------------------------------------------- card 02
def tokenize_prompt_and_output(
    prompt_strs: list[str],
    output_strs: list[str],
    tokenizer,
) -> dict[str, torch.Tensor]:
    """Tokenize prompt and response separately, concatenate with NO special tokens,
    and build a response_mask aligned with the shifted labels."""
    assert len(prompt_strs) == len(output_strs)
    prompt_ids_list = [
        tokenizer(p, add_special_tokens=False)["input_ids"] for p in prompt_strs
    ]
    output_ids_list = [
        tokenizer(o, add_special_tokens=False)["input_ids"] for o in output_strs
    ]
    seqs = [p + o for p, o in zip(prompt_ids_list, output_ids_list)]
    max_len = max(len(s) for s in seqs)
    pad_id = getattr(tokenizer, "pad_token_id", None)
    if pad_id is None:
        pad_id = getattr(tokenizer, "eos_token_id", 0) or 0

    batch = len(seqs)
    padded = torch.full((batch, max_len), pad_id, dtype=torch.long)
    resp_mask_full = torch.zeros((batch, max_len), dtype=torch.bool)
    for i, (p_ids, seq) in enumerate(zip(prompt_ids_list, seqs)):
        padded[i, : len(seq)] = torch.tensor(seq, dtype=torch.long)
        resp_mask_full[i, len(p_ids) : len(seq)] = True

    input_ids = padded[:, :-1]
    labels = padded[:, 1:]
    # Aligned with labels: position t holds the label from original position t+1.
    response_mask = resp_mask_full[:, 1:]
    return {"input_ids": input_ids, "labels": labels, "response_mask": response_mask}


# --------------------------------------------------------------- card 03
def get_response_log_probs(
    model,
    input_ids: torch.Tensor,
    labels: torch.Tensor,
    return_token_entropy: bool = False,
) -> dict[str, torch.Tensor]:
    """Per-token conditional log-probs log p(labels_t | x_<t); optional entropies."""
    logits = model(input_ids).logits.float()
    log_probs_full = F.log_softmax(logits, dim=-1)
    log_probs = log_probs_full.gather(-1, labels.unsqueeze(-1)).squeeze(-1)
    result = {"log_probs": log_probs}
    if return_token_entropy:
        with torch.no_grad():
            probs = log_probs_full.exp()
            result["token_entropy"] = -(probs * log_probs_full).sum(-1)
    return result


# --------------------------------------------------------------- card 04
def compute_rollout_rewards(
    reward_fn: Callable[[str, str], dict[str, float]],
    rollout_responses: list[str],
    repeated_ground_truths: list[str],
) -> tuple[torch.Tensor, dict[str, float]]:
    """Score each rollout; ground truths are repeated group_size times, aligned."""
    assert len(rollout_responses) == len(repeated_ground_truths)
    rewards, format_rewards, answer_rewards = [], [], []
    for response, gt in zip(rollout_responses, repeated_ground_truths):
        scores = reward_fn(response, gt)
        rewards.append(float(scores["reward"]))
        format_rewards.append(float(scores.get("format_reward", 0.0)))
        answer_rewards.append(float(scores.get("answer_reward", 0.0)))
    raw_rewards = torch.tensor(rewards, dtype=torch.float32)
    metadata = {
        "mean_reward": raw_rewards.mean().item(),
        "mean_format_reward": sum(format_rewards) / len(format_rewards),
        "mean_answer_reward": sum(answer_rewards) / len(answer_rewards),
        "max_reward": raw_rewards.max().item(),
        "min_reward": raw_rewards.min().item(),
    }
    return raw_rewards, metadata


# --------------------------------------------------------------- card 05
def compute_group_normalized_rewards(
    raw_rewards: torch.Tensor,
    group_size: int,
    baseline: Literal["mean", "none"] = "mean",
    advantage_eps: float = 1e-6,
    advantage_normalizer: Literal["std", "none", "mean"] = "std",
) -> tuple[torch.Tensor, dict[str, float]]:
    """GRPO (std) / Dr.GRPO (none) / MaxRL (mean) advantage computation."""
    if raw_rewards.numel() % group_size != 0:
        raise ValueError("raw_rewards length must be divisible by group_size")
    grouped = raw_rewards.view(-1, group_size)
    mu = grouped.mean(dim=1, keepdim=True)
    # torch.std default (Bessel-corrected), as the handout mandates.
    std = grouped.std(dim=1, keepdim=True)

    if baseline == "mean":
        adv = grouped - mu
    elif baseline == "none":
        adv = grouped.clone()
    else:
        raise NotImplementedError(f"baseline={baseline}")

    if advantage_normalizer == "std":
        adv = adv / (std + advantage_eps)
    elif advantage_normalizer == "mean":
        adv = adv / (mu + advantage_eps)
    elif advantage_normalizer == "none":
        pass
    else:
        raise NotImplementedError(f"advantage_normalizer={advantage_normalizer}")

    metadata = {
        "reward_mean": raw_rewards.mean().item(),
        "reward_std": raw_rewards.std().item(),
        "group_reward_mean_min": mu.min().item(),
        "group_reward_mean_max": mu.max().item(),
    }
    return adv.reshape(-1), metadata


# --------------------------------------------------------------- card 06
def compute_policy_gradient_loss(
    raw_rewards_or_advantages: torch.Tensor,
    policy_log_probs: torch.Tensor,
    importance_reweighting_method: Literal["none", "noclip", "grpo", "gspo"] = "none",
    old_log_probs: torch.Tensor | None = None,
    cliprange: float | None = None,
    response_mask: torch.Tensor | None = None,
) -> tuple[torch.Tensor, dict[str, torch.Tensor]]:
    """Per-token policy-gradient loss (negative objective, ready for backward)."""
    advantages = raw_rewards_or_advantages.reshape(-1, 1).to(policy_log_probs.dtype)
    metadata: dict[str, torch.Tensor] = {}

    if importance_reweighting_method == "none":
        objective = advantages * policy_log_probs
        return -objective, metadata

    if old_log_probs is None:
        raise ValueError("old_log_probs is required for off-policy methods")
    old_log_probs = old_log_probs.detach()

    if importance_reweighting_method == "noclip":
        # d/dtheta (A * w_t) = A * w_t * dlog pi -- the token-reweighted estimator.
        weights = torch.exp(policy_log_probs - old_log_probs)
        objective = advantages * weights
        return -objective, metadata

    if cliprange is None:
        raise ValueError("cliprange is required for clipped methods")

    if importance_reweighting_method == "grpo":
        weights = torch.exp(policy_log_probs - old_log_probs)
        unclipped = advantages * weights
        clipped = advantages * torch.clamp(weights, 1.0 - cliprange, 1.0 + cliprange)
        objective = torch.minimum(unclipped, clipped)
        with torch.no_grad():
            metadata["clip_fraction"] = (clipped < unclipped).float().mean()
        return -objective, metadata

    if importance_reweighting_method == "gspo":
        if response_mask is None:
            raise ValueError("response_mask is required for gspo")
        mask = response_mask.to(policy_log_probs.dtype)
        diff = (policy_log_probs - old_log_probs) * mask
        denom = mask.sum(dim=-1, keepdim=True).clamp_min(1.0)
        # Geometric mean importance weight, computed stably in log space.
        seq_weight = torch.exp(diff.sum(dim=-1, keepdim=True) / denom)
        unclipped = advantages * seq_weight
        clipped = advantages * torch.clamp(seq_weight, 1.0 - cliprange, 1.0 + cliprange)
        objective = torch.minimum(unclipped, clipped)  # (B, 1), broadcast over tokens
        with torch.no_grad():
            metadata["clip_fraction"] = (clipped < unclipped).float().mean()
        return -objective.expand_as(policy_log_probs), metadata

    raise NotImplementedError(
        f"importance_reweighting_method={importance_reweighting_method}"
    )


# --------------------------------------------------------------- card 07
def aggregate_loss_across_microbatch(
    per_token_policy_gradient_loss: torch.Tensor,
    mask: torch.Tensor,
    loss_normalization: Literal["sequence", "constant"] = "sequence",
    normalization_constant: int | None = None,
) -> torch.Tensor:
    """sequence: mean over tokens within each sequence, then mean over sequences.
    constant: masked sum divided by a fixed constant."""
    mask = mask.to(per_token_policy_gradient_loss.dtype)
    masked = per_token_policy_gradient_loss * mask
    if loss_normalization == "sequence":
        per_seq = masked.sum(dim=-1) / mask.sum(dim=-1).clamp_min(1.0)
        return per_seq.mean()
    if loss_normalization == "constant":
        if normalization_constant is None:
            raise ValueError("normalization_constant required for constant normalization")
        return masked.sum() / normalization_constant
    raise NotImplementedError(f"loss_normalization={loss_normalization}")


# --------------------------------------------------------------- card 08
def grpo_train_step(
    model,
    tokenizer,
    optimizer: torch.optim.Optimizer,
    gradient_accumulation_steps: int,
    max_grad_norm: float | None,
    reward_fn: Callable[[str, str], dict[str, float]],
    repeated_prompts: list[str],
    rollout_responses: list[str],
    repeated_ground_truths: list[str],
    group_size: int,
    # Reward normalization
    baseline: Literal["mean", "none"] = "mean",
    advantage_eps: float = 1e-6,
    advantage_normalizer: Literal["std", "none", "mean"] = "std",
    # Importance reweighting and clipping
    importance_reweighting_method: Literal["none", "noclip", "grpo", "gspo"] = "none",
    old_log_probs: torch.Tensor | None = None,
    cliprange: float | None = None,
    # Loss normalization
    loss_normalization: Literal["sequence", "constant"] = "sequence",
    normalization_constant: int | None = None,
    # Speed: skip zero-advantage sequences (gradient-equivalent)
    prune_zero_advantage: bool = True,
    device: str | torch.device = "cpu",
) -> tuple[torch.Tensor, dict[str, float]]:
    """One full policy-gradient update: rewards -> advantages -> microbatched
    forward/backward with gradient accumulation -> clip -> optimizer step."""
    raw_rewards, reward_meta = compute_rollout_rewards(
        reward_fn, rollout_responses, repeated_ground_truths
    )
    advantages, _ = compute_group_normalized_rewards(
        raw_rewards, group_size, baseline, advantage_eps, advantage_normalizer
    )

    batch = tokenize_prompt_and_output(repeated_prompts, rollout_responses, tokenizer)
    input_ids = batch["input_ids"].to(device)
    labels = batch["labels"].to(device)
    response_mask = batch["response_mask"].to(device)
    advantages = advantages.to(device)
    if old_log_probs is not None:
        old_log_probs = old_log_probs.to(device)

    n_total = input_ids.shape[0]
    keep = torch.arange(n_total, device=device)
    if prune_zero_advantage:
        nonzero = advantages != 0
        if nonzero.any() and not nonzero.all():
            keep = nonzero.nonzero(as_tuple=True)[0]
            frac_kept = len(keep) / n_total
            # Shrink accumulation steps by the pruned fraction (microbatch size fixed).
            gradient_accumulation_steps = max(
                1, round(gradient_accumulation_steps * frac_kept)
            )

    n_kept = len(keep)
    microbatch_size = max(1, n_kept // gradient_accumulation_steps)

    optimizer.zero_grad()
    total_loss = 0.0
    entropy_sum, entropy_count = 0.0, 0
    clip_fractions: list[float] = []

    for start in range(0, n_kept, microbatch_size):
        idx = keep[start : start + microbatch_size]
        out = get_response_log_probs(
            model, input_ids[idx], labels[idx], return_token_entropy=True
        )
        log_probs = out["log_probs"]
        micro_mask = response_mask[idx]
        per_token_loss, loss_meta = compute_policy_gradient_loss(
            advantages[idx],
            log_probs,
            importance_reweighting_method,
            old_log_probs[idx] if old_log_probs is not None else None,
            cliprange,
            micro_mask,
        )
        loss = aggregate_loss_across_microbatch(
            per_token_loss, micro_mask, loss_normalization, normalization_constant
        )
        if loss_normalization == "sequence":
            # Reweight so accumulated microbatch gradients equal the full-batch gradient.
            # Zero-advantage sequences contribute 0 loss but count in the denominator,
            # so we divide by the *original* batch size.
            loss = loss * (len(idx) / n_total)
        # constant normalization: microbatch losses are directly additive.
        loss.backward()
        total_loss += loss.item()

        with torch.no_grad():
            ent = out["token_entropy"]
            entropy_sum += (ent * micro_mask).sum().item()
            entropy_count += micro_mask.sum().item()
        if "clip_fraction" in loss_meta:
            clip_fractions.append(float(loss_meta["clip_fraction"]))

    grad_norm = None
    if max_grad_norm is not None:
        grad_norm = torch.nn.utils.clip_grad_norm_(model.parameters(), max_grad_norm)
    optimizer.step()
    optimizer.zero_grad()

    metadata: dict[str, float] = {
        "loss": total_loss,
        "grad_norm": float(grad_norm) if grad_norm is not None else float("nan"),
        "token_entropy": entropy_sum / max(entropy_count, 1),
        "n_sequences_used": n_kept,
        **reward_meta,
    }
    if clip_fractions:
        metadata["clip_fraction"] = sum(clip_fractions) / len(clip_fractions)
    return torch.tensor(total_loss), metadata
