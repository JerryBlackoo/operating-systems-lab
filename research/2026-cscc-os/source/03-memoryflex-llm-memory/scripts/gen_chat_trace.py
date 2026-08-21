"""多轮对话/agent 工作负载生成器（阶段4：集群调度评测）

生成带共享 system prompt、用户历史与主题复用的请求流：
  {user, topic, prompt}  其中 prompt = system + 历史 + 新提问

用法: python scripts/gen_chat_trace.py --out results/trace_chat.jsonl --users 3 --turns 8
"""

import argparse
import json
import random
from pathlib import Path


def system_for(user):
    return (f"你是部署在边缘设备上的专属助手，服务用户 {user}。你的运行环境为 "
            "6GB 显存、16GB 内存的笔记本级设备，推理引擎采用虚拟内存按需加载、"
            "页级专家锁定与异步预取，由集群调度器按 KV 命中与负载均衡路由。"
            "请用简洁准确的中文回答，先结论、再机制、后参数示例，两百字以内。")

TOPIC_TEXTS = {
    "t1": "请解释大语言模型推理中的虚拟内存与按需加载机制，包括缺页与换出的代价。",
    "t2": "请说明 KV Cache 在多轮对话中的作用，以及前缀缓存如何降低预填充开销。",
}

FOLLOW_UPS = [
    "请进一步说明预取如何隐藏 IO 延迟。",
    "量化精度 Q4 与 KV q8 对内存和效果有何影响？",
    "在 6GB 显存设备上如何部署 7B 模型？",
    "负载均衡与 KV 命中如何同时兼顾？",
    "请给出一个具体的参数例子。",
    "这对首 token 延迟有什么帮助？",
    "边缘集群与云集群的调度差异是什么？",
    "总结一下上述要点。",
]


def gen(users, turns, seed=42):
    rng = random.Random(seed)
    reqs = []
    for u in range(users):
        topic = f"t{u % 2 + 1}"
        history = []
        sysp = system_for(f"u{u}")
        for t in range(turns):
            if t == 0:
                question = TOPIC_TEXTS[topic]
            else:
                question = FOLLOW_UPS[(u + t) % len(FOLLOW_UPS)]
            # 完整历史（多轮对话前缀随轮次增长，放大 KV 复用差异）
            hist = "\n".join(history)
            prompt = f"{sysp}\n用户历史：{hist}\n新问题：{question}"
            reqs.append({"user": f"u{u}", "topic": topic, "prompt": prompt,
                         "turn": t})
            history.append(f"问：{question} 答：好的，我来回答。")
    # 随机到达（真实流量）：前缀命中依赖调度器把同用户请求聚合到同一实例
    rng.shuffle(reqs)
    return reqs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="results/trace_chat.jsonl")
    ap.add_argument("--users", type=int, default=3)
    ap.add_argument("--turns", type=int, default=8)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    reqs = gen(args.users, args.turns, args.seed)
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as f:
        for r in reqs:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"wrote {len(reqs)} requests -> {args.out}")


if __name__ == "__main__":
    main()
