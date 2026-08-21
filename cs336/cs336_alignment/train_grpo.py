"""GRPO training loop (card notes/cs336-a5/09-grpo-training-loop.md).

Faithful to the handout's two-GPU layout: HF policy + AdamW on cuda:0,
vLLM rollouts on cuda:1 with weight syncing. Requires GPUs, transformers,
and vllm; the algorithmic components it drives are fully unit-tested on CPU
(see tests/test_a5_grpo.py).

Usage:
    python -m cs336_alignment.train_grpo --model allenai/OLMo-2-0425-1B \
        --train-jsonl data/gsm8k/train.jsonl --val-jsonl data/gsm8k/test.jsonl
"""

from __future__ import annotations

import argparse
import json
import random

import torch

from .grader import extract_gsm8k_answer, r1_zero_reward_fn
from .grpo import get_response_log_probs, grpo_train_step, tokenize_prompt_and_output
from .prompts import R1_ZERO_PROMPT, R1_ZERO_SAMPLING_PARAMS


def parse_args(argv=None):
    p = argparse.ArgumentParser(description="On/off-policy GRPO on GSM8K")
    p.add_argument("--model", default="allenai/OLMo-2-0425-1B")
    p.add_argument("--train-jsonl", required=True)
    p.add_argument("--val-jsonl", required=True)
    p.add_argument("--n-train-examples", type=int, default=6400)
    p.add_argument("--n-val-examples", type=int, default=1024)
    p.add_argument("--num-rollout-steps", type=int, default=200)
    p.add_argument("--learning-rate", type=float, default=1e-5)
    p.add_argument("--rollout-batch-size", type=int, default=256)  # counts responses
    p.add_argument("--train-batch-size", type=int, default=256)
    p.add_argument("--group-size", type=int, default=8)
    p.add_argument("--gradient-accumulation-steps", type=int, default=32)
    p.add_argument("--max-grad-norm", type=float, default=1.0)
    p.add_argument("--baseline", default="mean", choices=["mean", "none"])
    p.add_argument("--advantage-normalizer", default="std", choices=["std", "none", "mean"])
    p.add_argument("--loss-normalization", default="sequence", choices=["sequence", "constant"])
    p.add_argument("--importance-reweighting", default="none",
                   choices=["none", "noclip", "grpo", "gspo"])
    p.add_argument("--cliprange", type=float, default=0.2)
    p.add_argument("--eval-interval", type=int, default=10)
    p.add_argument("--log-rollouts-interval", type=int, default=40)
    p.add_argument("--seed", type=int, default=0)
    p.add_argument("--policy-device", default="cuda:0")
    p.add_argument("--vllm-gpu", type=int, default=1)
    p.add_argument("--wandb-project", default=None)
    return p.parse_args(argv)


def load_gsm8k(path: str, limit: int) -> list[dict]:
    examples = []
    with open(path) as f:
        for line in f:
            row = json.loads(line)
            examples.append(
                {"question": row["question"], "answer": extract_gsm8k_answer(row["answer"])}
            )
            if len(examples) >= limit:
                break
    return examples


def main(argv=None):  # pragma: no cover - requires GPUs + vllm
    args = parse_args(argv)
    random.seed(args.seed)
    torch.manual_seed(args.seed)

    from transformers import AutoModelForCausalLM, AutoTokenizer

    try:
        from cs336_alignment.vllm_utils import VLLMServer  # course-provided helper
    except ImportError as e:
        raise SystemExit(
            "train_grpo requires the course vllm_utils (vLLM server with weight sync). "
            "Copy cs336_alignment/vllm_utils.py from the official assignment5 repo."
        ) from e

    policy = AutoModelForCausalLM.from_pretrained(
        args.model, torch_dtype=torch.bfloat16, attn_implementation="flash_attention_2"
    ).to(args.policy_device)
    tokenizer = AutoTokenizer.from_pretrained(args.model)
    optimizer = torch.optim.AdamW(
        policy.parameters(), lr=args.learning_rate, betas=(0.9, 0.95), weight_decay=0.0
    )

    train_set = load_gsm8k(args.train_jsonl, args.n_train_examples)
    val_set = load_gsm8k(args.val_jsonl, args.n_val_examples)

    server = VLLMServer(model_id=args.model, gpu=args.vllm_gpu, seed=args.seed)
    server.start()
    server.init_weight_sync(args.policy_device)

    wandb_run = None
    if args.wandb_project:
        import wandb

        wandb_run = wandb.init(project=args.wandb_project, config=vars(args))

    n_prompts = args.rollout_batch_size // args.group_size
    off_policy_ratio = args.rollout_batch_size // args.train_batch_size

    for step in range(args.num_rollout_steps):
        # 1) Sync weights, then sample rollouts from the *current* policy.
        server.sync_policy_weights(policy)
        batch_examples = random.sample(train_set, n_prompts)
        repeated_prompts, repeated_gts = [], []
        for ex in batch_examples:
            prompt = R1_ZERO_PROMPT.format(question=ex["question"])
            repeated_prompts += [prompt] * args.group_size
            repeated_gts += [ex["answer"]] * args.group_size
        completions = server.generate_completions(repeated_prompts, R1_ZERO_SAMPLING_PARAMS)
        responses = [c.text for c in completions]

        # 2) Off-policy: compute old log-probs once, then take several train steps.
        old_log_probs = None
        if args.importance_reweighting != "none":
            with torch.no_grad():
                batch = tokenize_prompt_and_output(repeated_prompts, responses, tokenizer)
                old_log_probs = get_response_log_probs(
                    policy,
                    batch["input_ids"].to(args.policy_device),
                    batch["labels"].to(args.policy_device),
                )["log_probs"].cpu()

        for sub in range(off_policy_ratio):
            lo = sub * args.train_batch_size
            hi = lo + args.train_batch_size
            loss, meta = grpo_train_step(
                policy,
                tokenizer,
                optimizer,
                gradient_accumulation_steps=args.gradient_accumulation_steps,
                max_grad_norm=args.max_grad_norm,
                reward_fn=r1_zero_reward_fn,
                repeated_prompts=repeated_prompts[lo:hi],
                rollout_responses=responses[lo:hi],
                repeated_ground_truths=repeated_gts[lo:hi],
                group_size=args.group_size,
                baseline=args.baseline,
                advantage_normalizer=args.advantage_normalizer,
                importance_reweighting_method=args.importance_reweighting,
                old_log_probs=old_log_probs[lo:hi] if old_log_probs is not None else None,
                cliprange=args.cliprange,
                loss_normalization=args.loss_normalization,
                normalization_constant=args.train_batch_size * 512,
                device=args.policy_device,
            )
            print(f"step {step} sub {sub} | loss {float(loss):.4f} | {meta}")
            if wandb_run:
                wandb_run.log({"train/" + k: v for k, v in meta.items()})

        # 3) Periodic validation on >= 1024 examples (CoT evals are noisy).
        if step % args.eval_interval == 0:
            server.sync_policy_weights(policy)
            val_prompts = [R1_ZERO_PROMPT.format(question=ex["question"]) for ex in val_set]
            outs = server.generate_completions(val_prompts, R1_ZERO_SAMPLING_PARAMS)
            rewards = [
                r1_zero_reward_fn(c.text, ex["answer"])["reward"]
                for c, ex in zip(outs, val_set)
            ]
            acc = sum(rewards) / len(rewards)
            lengths = [len(c.token_ids) for c in outs]
            print(f"step {step} | val_accuracy {acc:.4f} | mean_len {sum(lengths)/len(lengths):.1f}")
            if wandb_run:
                wandb_run.log({"val/accuracy": acc, "step": step})
        if step % args.log_rollouts_interval == 0:
            print("SAMPLE ROLLOUT:", responses[0][:2000])


if __name__ == "__main__":
    main()
