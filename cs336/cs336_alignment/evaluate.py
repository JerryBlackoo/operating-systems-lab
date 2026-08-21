"""Prompting baselines evaluation (card notes/cs336-a5/01-prompting-baselines.md).

Requires a GPU + vllm for the real OLMo-2-0425-1B runs; the grading and
categorization logic below is model-independent and unit-tested on CPU.
"""

from __future__ import annotations

import argparse
import json
from collections import Counter

from .grader import extract_gsm8k_answer, question_only_reward_fn, r1_zero_reward_fn
from .prompts import (
    QUESTION_ONLY_PROMPT,
    QUESTION_ONLY_SAMPLING_PARAMS,
    R1_ZERO_PROMPT,
    R1_ZERO_SAMPLING_PARAMS,
)

PROMPTS = {
    "question_only": (QUESTION_ONLY_PROMPT, question_only_reward_fn, QUESTION_ONLY_SAMPLING_PARAMS),
    "r1_zero": (R1_ZERO_PROMPT, r1_zero_reward_fn, R1_ZERO_SAMPLING_PARAMS),
}


def categorize(reward: dict[str, float]) -> str:
    """Handout categories: (1) both rewards 1, (2) format 1 answer 0, (3) both 0."""
    if reward["format_reward"] == 1 and reward["answer_reward"] == 1:
        return "correct"
    if reward["format_reward"] == 1:
        return "format_only"
    return "neither"


def evaluate_responses(responses: list[str], ground_truths: list[str], reward_fn) -> dict:
    counts: Counter = Counter()
    for response, gt in zip(responses, ground_truths):
        counts[categorize(reward_fn(response, gt))] += 1
    total = sum(counts.values())
    return {
        "n": total,
        "accuracy": counts["correct"] / total if total else 0.0,
        **{k: counts.get(k, 0) for k in ("correct", "format_only", "neither")},
    }


def main(argv=None):  # pragma: no cover - requires GPU + vllm
    p = argparse.ArgumentParser()
    p.add_argument("--model", default="allenai/OLMo-2-0425-1B")
    p.add_argument("--test-jsonl", required=True)
    p.add_argument("--prompt", default="r1_zero", choices=list(PROMPTS))
    p.add_argument("--limit", type=int, default=None)
    args = p.parse_args(argv)

    from cs336_alignment.vllm_utils import VLLMServer

    template, reward_fn, sampling_params = PROMPTS[args.prompt]
    examples = []
    with open(args.test_jsonl) as f:
        for line in f:
            row = json.loads(line)
            examples.append((row["question"], extract_gsm8k_answer(row["answer"])))
            if args.limit and len(examples) >= args.limit:
                break

    server = VLLMServer(model_id=args.model)
    server.start()
    prompts = [template.format(question=q) for q, _ in examples]
    completions = server.generate_completions(prompts, sampling_params)
    metrics = evaluate_responses(
        [c.text for c in completions], [gt for _, gt in examples], reward_fn
    )
    print(json.dumps(metrics, indent=2))


if __name__ == "__main__":
    main()
