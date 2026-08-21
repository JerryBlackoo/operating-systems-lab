"""CS336 Assignment 5 (alignment): GRPO reasoning RL components.

Implemented following the study cards in notes/cs336-a5/.
"""

from .grpo import (
    tokenize_prompt_and_output,
    get_response_log_probs,
    compute_rollout_rewards,
    compute_group_normalized_rewards,
    compute_policy_gradient_loss,
    aggregate_loss_across_microbatch,
    grpo_train_step,
)
from .prompts import R1_ZERO_PROMPT, QUESTION_ONLY_PROMPT
from .grader import r1_zero_reward_fn, question_only_reward_fn, extract_gsm8k_answer

__all__ = [
    "tokenize_prompt_and_output",
    "get_response_log_probs",
    "compute_rollout_rewards",
    "compute_group_normalized_rewards",
    "compute_policy_gradient_loss",
    "aggregate_loss_across_microbatch",
    "grpo_train_step",
    "R1_ZERO_PROMPT",
    "QUESTION_ONLY_PROMPT",
    "r1_zero_reward_fn",
    "question_only_reward_fn",
    "extract_gsm8k_answer",
]
