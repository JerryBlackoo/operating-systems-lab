# 01 · 提示词基线评测（prompting_baselines）

讲义 §3 ｜ 5 分 ｜ 无 pytest（交付物是评测脚本 + 指标分析）

```text
Problem:
prompting_baselines — 评测 OLMo-2-0425-1B 在 GSM8K 上的三种提示词表现

输入:
- 模型: allenai/OLMo-2-0425-1B（基座模型，vLLM 推理）
- 数据: data/gsm8k/test.jsonl（{"question": ..., "answer": "...#### 72"}）
- 三种 prompt（cs336_alignment/prompts/ 下）:
  question_only:  "{question} Please put your final answer within \boxed{}."
  r1_zero:        R1-Zero 风格 CoT，以 "Assistant: <think>" 结尾
  r1_zero_three_shot: 前面多 3 个 (question, reasoning, answer) 示例

输出:
每种 prompt 的三类统计:
(1) format=1 且 answer=1（全对）
(2) format=1 且 answer=0（格式对答案错——含解析失败的"假错"）
(3) format=0 且 answer=0（格式就不对）

参数:
采样: temperature=1.0, top_p=1.0, max_tokens=512
r1 系 prompt 额外: stop=["</answer>"], include_stop_str_in_output=True

公式:
GT 提取: gsm8k 的 answer 字段形如 "{rationale} #### {answer}"
         -> answer.split("####")[-1].strip()
评分: r1 系用 drgrpo_grader.r1_zero_reward_fn(response, gt)
      question_only 用 drgrpo_grader.question_only_reward_fn（找 \boxed{}）
      返回 {"reward", "format_reward", "answer_reward"}，无部分分

步骤:
1. server = VLLMServer(model_id=...)；server.start()
2. 读 jsonl，把 question 填进 prompt 模板，抽取 GT 答案
3. completions = server.generate_completions(prompts, sampling_params)
4. 对每条 completion 调对应 reward_fn，按三类统计
5. 抽 10 个第(2)类样本人工检查: 是真错还是解析问题？第(3)类同理

需要查的 API:
- cs336_alignment.vllm_utils.VLLMServer / generate_completions
- cs336_alignment.drgrpo_grader.r1_zero_reward_fn / question_only_reward_fn
- json.loads（逐行 jsonl）
- str.format / f-string（prompt 模板填充）

边界情况:
- stop 串只给 r1 系 prompt 用；question_only 不加
- r1_zero prompt 已含开头 <think>，模型生成从 thinking 开始
- reward_fn 的 ground_truth 参数只要"答案"，不要整个 rationale
- 基座模型对 question_only 常常不好好答题（会续写/跑题）——这正是
  观察点之一
- include_stop_str_in_output=True 才能让 </answer> 进入输出供解析

测试:
- 无 pytest；自检:
- 三种 prompt 的准确率是否符合预期量级（few-shot > zero-shot CoT >
  question_only 或类似趋势）
- 第(2)类中有多少其实是答案对但没被解析出——评分器的假阴性率
```
