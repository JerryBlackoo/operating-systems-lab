"""Prompt templates from the A5 handout (card notes/cs336-a5/01)."""

R1_ZERO_PROMPT = """A conversation between User and Assistant. The User asks a question, and the Assistant solves it. The Assistant first thinks about the reasoning process in the mind and then provides the User with the answer. The reasoning process is enclosed within <think> </think> and the answer is enclosed within <answer> </answer> tags, respectively, i.e., <think> reasoning process here </think> <answer> answer here </answer>.

User: {question}
Assistant: <think>"""

QUESTION_ONLY_PROMPT = "{question} Please put your final answer within \\boxed{{}}."

# Sampling parameters used for the r1_zero prompts (handout §3.4).
R1_ZERO_SAMPLING_PARAMS = {
    "temperature": 1.0,
    "top_p": 1.0,
    "max_tokens": 512,
    "stop": ["</answer>"],
    "include_stop_str_in_output": True,
}

QUESTION_ONLY_SAMPLING_PARAMS = {
    "temperature": 1.0,
    "top_p": 1.0,
    "max_tokens": 512,
}
