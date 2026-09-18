# 2026 CSCC OS `proj32` 横向对比：eBPF × 多智能体异常监测

本目录比较 2026 全国大学生计算机系统能力大赛操作系统设计赛功能挑战赛 `proj32` 的五支公开项目：

- **看月亮 / MtAgentLens** — 国防科技大学 — `T202690002999777`
- **光明未来** — 国防科技大学 — `T2026900029910646`
- **三个诸葛亮** — 国防科技大学 — `T2026900029911547`
- **单击输入文本** — 大连民族大学 — `T2026120269910095`
- **愿为莫离** — 武汉大学 — `T2026104869910476`

> 判定原则：表中只把可以从公开源码、README、设计文档或希冀平台文件记录直接证明的能力标为 ✅。同样参加 `proj32` 并不代表所有队伍都完成了赛题的所有目标。
>
> 图例：**✅ = 直接确认**；**◐ = 有近似/部分能力，但抽象或完成度低于该行含义**；**? = 当前公开索引不足，等待完整源码快照核验**；**— = 当前已读源码中未见明确实现**。

## 能力矩阵

| 能力 | MtAgentLens | 光明未来 | 三个诸葛亮 | 单击输入文本 | 愿为莫离 |
|---|---|---|---|---|---|
| **eBPF probes** | ✅ 多组探针：TLS / process / stdio / file / browser | ✅ 项目明确为 eBPF 多智能体监测；细粒度探针待完整源码核验 | ✅ tracepoint + kprobe + uprobe，`main.bpf.c` | ✅ `kernel/stage1` / `stage2` BPF，设计文档含探针表 | ✅ `proj32` eBPF 赛题实现；细粒度探针待完整源码核验 |
| **TLS 明文** | ✅ `SSL_read` / `SSL_write` uprobe；兼容静态链接 BoringSSL 的 `--binary-path` | ? | ✅ `SSL_read` / `SSL_write` uprobes | ✅ 存在独立 `--ssl-bpf` / stage2 SSL BPF 路径 | ? |
| **Agent 身份识别** | ✅ `root_pid + tls_session + conversation_id`，Union-Find 合并成 canonical agent | ? | ✅/◐ 自动识别 Claude Code、Codex、Gemini CLI、Kiro、Cursor、Copilot、Aider；更接近进程/产品识别 | ◐ 有 PID/TID 与 Prompt 上下文归因；未见三信号身份融合证据 | ? |
| **多进程关联** | ✅ 进程树 + Agent delegation + IdentityResolver | ? | ◐ 跟踪 `execve/fork/clone`，有 agent tracker / cross-layer correlator | ◐ 有进程与系统调用 Pipeline；完整跨进程身份连续性待核验 | ? |
| **因果图** | ✅ `ResponsibilityGraph`，节点/边/图简化/责任链 | ? | ◐ 有 Prompt→底层操作“因果链”和 `/api/causal-links`，未见 MtAgentLens 等价责任图引擎 | ? | ? |
| **污点传播** | ✅ `TaintTracker` 跨进程/跨 Agent 传播 | ? | — 当前公开镜像 README/进度文档未见明确 taint engine | ? | ? |
| **根因分析** | ✅ `TracebackBuilder → ClaimBuilder → AlertBuilder`，输出责任主体与证据链 | ? | ◐ 告警归因 + Prompt 上下文关联；未见等价 traceback/claim 系统 | ◐ 有 Alert JSONL / 上下文，深层根因回溯待核验 | ? |
| **LLM** | ✅ Observer LLM，仅冲突信号时介入；L1 精确 + L2 语义 CachePrior | ? | — 分析侧公开实现是规则 + 统计 + ML 分类器，未见 LLM judge | ? | ? |
| **Benchmark** | ✅ 自建 **FuxiBench：86 场景**（67 attack / 19 benign） | ? | ◐ demo simulator + 测试；未见同等级冻结 benchmark | ◐ 有 `docs/performance.md` 性能基准报告与典型场景体系，但公开索引显示至少早期版本仍是报告模板 | ? |
| **消融实验** | ✅ 四层消融；例如移除 file-read PBC 后覆盖率 `1.000 → 0.534` | ? | — 未见正式消融矩阵 | ? | ? |
| **性能开销** | ✅ probe 纳秒级；总 probe 约 218 ns/op；吞吐开销 `<3%`；RSS 约 36.8 MB | ? | ? README 未给系统开销定量结果 | ◐ 有性能文档入口，公开索引暂未拿到最终定量结果 | ? |

## 1. MtAgentLens：为什么明显更完整

MtAgentLens 的突出点不是“有更多 eBPF probe”，而是从 **OS 事件**继续向上构造了一个 Agent 语义层：

```text
raw eBPF event
→ timestamp / HTTP / SSE normalization
→ Process Boundary Crossing (PBC)
→ ResponsibilityGraph
→ IdentityResolver
→ TaintTracker
→ conditional Observer LLM
→ Traceback / Claim / Alert
```

它解决了三个抽象断层：

1. **PID ≠ Agent**：通过 `root_pid / tls_session / conversation_id` 多信号融合保持 Agent 身份连续性。
2. **事件 ≠ 因果**：把 Process / File / NetworkEndpoint / Intent / Action 组织成 ResponsibilityGraph。
3. **异常 ≠ 根因**：通过污点传播和 traceback 将最后的危险操作反向追到上游 Agent、handoff 与输入来源。

此外，它把“论文式证据链”也做完了：FuxiBench、消融、开销测试、回放、SQLite+JSONL 持久化以及前端调查界面。这也是目前五个项目中公开证据最完整的一支。

### 直接证据

- GitLab 国赛仓：`https://gitlab.eduxiji.net/educg-group-43734-3136859/T202690002999777-3686`
- GitHub 公开镜像：`https://github.com/LiuZhouKang/MtAgentLens`

## 2. 三个诸葛亮：更像“eBPF 多 Agent 可观测平台”

存在一个题目名高度一致的公开 GitHub 仓库：`neko4linux/eBPF-Multi-Agent`。当前将其作为**高匹配公开镜像候选**使用，但在没有 Team ID 直接写入仓库前，不把“GitHub 账号 ↔ 三个诸葛亮队”视为 100% 身份证明。

公开实现可确认：

- tracepoints：`execve`、`clone`、`openat`、`unlinkat`、`connect`
- kprobe：`tcp_connect`
- uprobes：`SSL_read` / `SSL_write`
- Ring Buffer / Hash Map / Per-CPU Array
- Agent tracker
- 规则/统计异常检测
- Prompt/Response parser
- ML classifier
- REST/WebSocket dashboard
- Prompt 与底层操作的 cross-layer causal link

它已经是一个完整的可观测系统，但从当前公开代码证据看，**“因果链接”还没有进一步升级成 MtAgentLens 那种统一 ResponsibilityGraph + canonical identity + cross-process taint + structured traceback 的责任归因模型**。

### 直接证据

- GitLab：`https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026900029911547-2892`
- 决赛阶段归档可见 `proj32-1-*`
- 高匹配 GitHub 镜像候选：`https://github.com/neko4linux/eBPF-Multi-Agent`

## 3. 单击输入文本：至少已经做到“两阶段 BPF + SSL”

希冀平台公开提交记录能确认该项目存在独立的 BPF 构建路径：

```text
kernel/stage1   # 基础 BPF 路径
kernel/stage2   # SSL BPF 路径
--bpf
--ssl-bpf
```

其文档提交还明确包含：

- `architecture.md`：架构图、探针表、Pipeline 数据流
- `usage.md`：CLI / 配置 / 3 个典型场景
- `api.md`：HTTP API 与 Alert JSONL schema
- `performance.md`：性能基准报告

所以它绝不是“只有一个 BPF demo”。不过当前搜索索引没有把最终版本的 Identity / Causal Graph / Taint / LLM / Ablation 实现正文完整暴露出来，这几项暂不猜。

### 直接证据

- 主仓：`https://gitlab.eduxiji.net/T2026120269910095/project3136859-382916`
- 初赛/阶段归档：`https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026120269910095-3080`
- 决赛归档在 `educg-net-48534-3136859 / proj32-单击输入文本-*`

## 4. 光明未来

希冀平台可以明确确认：

- 国防科技大学
- Team ID `T2026900029910646`
- 项目 `proj32-基于eBPF的多智能体异常监测系统`
- 主仓持续更新到 2026-08-18
- 许可证标记 GPL-2.0-or-later

但当前公开搜索索引没有暴露足够的 README / 关键源码正文，因此**不能仅凭项目标题把 TLS、因果图、污点、LLM、消融等能力全部打勾**。

### 直接证据

- 阶段归档：`https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026900029910646-3462`
- 决赛阶段归档：`educg-net-48534-3136859 / proj32-基于eBPF的多智能体异常监测系统-1053`

## 5. 愿为莫离

希冀平台可以明确确认：

- 武汉大学
- Team ID `T2026104869910476`
- 队伍 `愿为莫离`
- `proj32`
- 主项目持续更新到 2026-08-17，决赛归档更新到 2026-08-18

但当前网页索引只稳定暴露项目元数据，没有足够源码片段，因此高级能力暂标 `?`，等待源码导入后再做静态审计。

### 直接证据

- 阶段归档：`https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026104869910476-3357`
- 决赛归档：`https://gitlab.eduxiji.net/educg-group-48534-3136859/T2026104869910476-2464`

## 当前结论

如果只看“用了什么技术”，五支队伍其实都在同一个题面下竞争：

```text
eBPF → Agent observation → anomaly detection → attribution
```

真正拉开差距的是**向上抽象了多少层，并且有没有证据证明这些抽象真的有效**。

当前公开证据显示：

```text
基础观测
  eBPF / process / file / network / TLS
                    ↓
跨层关联
  Prompt ↔ PID ↔ syscall ↔ resource
                    ↓
身份连续性
  PID/session/conversation → Agent
                    ↓
因果模型
  ResponsibilityGraph
                    ↓
传播模型
  TaintTracker
                    ↓
责任归因
  Traceback / Claim / Alert
                    ↓
科学验证
  Benchmark / Ablation / Overhead
```

**MtAgentLens 的优势是这条链基本走到了底。**

而“三个诸葛亮”的公开镜像目前更明确地停留在“完整观测平台 + Agent 检测 + 异常规则/ML + Prompt→操作因果关联”；其余三队需要完整源码后才能公平判断。

## 源码导入状态

目标目录：`research/2026-cscc-os/proj32-comparison/sources/`

已经提交自动导入工作流：`.github/workflows/import-proj32-comparison.yml`。

当前 GitHub Actions 的最新 job 在分配 runner 前即失败（`steps=[]`, `runner_id=0`），因此**不能把源码标记为已导入**。工作流和所有已知 GitLab clone URL 已保留；一旦 GitHub Runner 可用，可直接重新运行完成 vendor 导入。

## 相关原创实验（不是 MtAgentLens）

本仓库另有一份独立的宿主教学原型：[`research/execution-scope/`](../../execution-scope/)。

它**不** vendor MtAgentLens 源码，也**不**声称已有真实 LSM/eBPF 强制。对比意图见该目录 README：MtAgentLens 侧重可观测与责任图；ExecutionScope 原型侧重 scope 传播、CausalOrigin ≠ AuthorityScope，以及模拟内核层的策略拒绝。
