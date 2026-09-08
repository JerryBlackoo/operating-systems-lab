"""Lightweight GSM8K graders (stand-in for the course's drgrpo_grader).

The official assignment ships cs336_alignment/drgrpo_grader.py with a
symbolic-math-aware parser. This module implements the same interface for
numeric GSM8K answers so the rest of the pipeline is runnable end to end:

    reward_fn(response, ground_truth) -> {"reward", "format_reward", "answer_reward"}

No partial credit: total reward = answer reward (format reward is for logging).
"""

from __future__ import annotations

import re

_NUMBER_RE = re.compile(r"-?\$?\d[\d,]*\.?\d*")


def extract_gsm8k_answer(answer_field: str) -> str:
    """GSM8K answers look like '{rationale} #### {answer}'."""
    return answer_field.split("####")[-1].strip()


def _normalize_number(text: str) -> str | None:
    m = _NUMBER_RE.findall(text.replace(",", "").replace("$", ""))
    if not m:
        return None
    value = m[-1]
    try:
        f = float(value)
    except ValueError:
        return None
    return str(int(f)) if f == int(f) else str(f)


def _grade_answer(candidate: str, ground_truth: str) -> bool:
    got = _normalize_number(candidate)
    want = _normalize_number(ground_truth)
    return got is not None and want is not None and got == want


def r1_zero_reward_fn(response: str, ground_truth: str) -> dict[str, float]:
    """Expects '...</think> <answer>...</answer>'. Format reward requires both tags."""
    answer_match = re.search(r"<answer>(.*?)</answer>", response, flags=re.DOTALL)
    format_ok = answer_match is not None and "</think>" in response
    answer_ok = bool(answer_match) and _grade_answer(answer_match.group(1), ground_truth)
    return {
        "reward": 1.0 if (format_ok and answer_ok) else 0.0,
        "format_reward": 1.0 if format_ok else 0.0,
        "answer_reward": 1.0 if answer_ok else 0.0,
    }


def question_only_reward_fn(response: str, ground_truth: str) -> dict[str, float]:
    """Expects the final answer inside \\boxed{...}."""
    boxed = re.findall(r"\\boxed\{([^{}]*)\}", response)
    format_ok = bool(boxed)
    answer_ok = bool(boxed) and _grade_answer(boxed[-1], ground_truth)
    return {
        "reward": 1.0 if (format_ok and answer_ok) else 0.0,
        "format_reward": 1.0 if format_ok else 0.0,
        "answer_reward": 1.0 if answer_ok else 0.0,
    }
