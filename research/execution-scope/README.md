# ExecutionScope 宿主教学原型

> 这是一份 **host 上的模拟 / 教学原型**，用来讲清楚 Agent 工具调用穿过异步任务、线程、回调和「子进程」之后，内核可见副作用该如何归因到**逻辑主体**，而不是 PID/TID。
>
> **不声称**已经实现真实 LSM / eBPF 内核强制。当前 `open` / `connect` / `unlink` / `exec` 全部走 `SimulatedKernel`，不会真正碰文件、网络或 `clone`。

对照笔记（MtAgentLens 等 proj32 可观测项目，**不要把本目录当成它的 vendor**）：

- [`../2026-cscc-os/proj32-comparison/README.md`](../2026-cscc-os/proj32-comparison/README.md)

## 要解决的问题

一次工具调用的因果链通常是：

```text
Agent → tool_call → Tool → spawn Task → background Task → syscall
```

中间会穿过 `asyncio` 任务、线程池、回调、模拟子进程。若内核只看 PID/TID：

1. **同一 PID 复用**（工作线程池）时，Agent A 的 `connect(github.com)` 和 Agent B 的 `unlink` 会糊成同一个进程；
2. **工具已经返回**后，后台任务仍可能发出 `connect(evil.com)` —— 事后归因丢失，或更糟：继续持有工具期间的权限（async privilege escape）。

本原型把「是谁造成的」和「现在还能否做」拆成两个对象。

## Causal vs Authority（延迟副作用）

| 对象 | 问题 | 生命周期 |
|------|------|----------|
| **CausalScope / CausalOrigin** | 谁造成了这次 syscall？ | 工具返回后 **仍然保留**，复制进每个 child |
| **AuthorityScope** | 现在还授权吗？ | 工具返回时可以 **expire**，继承了同一对象的后台任务立刻失去权限 |

典型例子（场景 `delayed_attribution`）：

1. Agent A 调用 SearchTool；SearchTool 的策略 **允许** 公网 `connect`。
2. 工具在返回前 `spawn_background(connect(evil.com), inherit_authority=True, after_tool_return=True)`。
3. SearchTool 返回 → `AuthorityScope.expire()`。
4. 后台任务真正执行 `connect(evil.com)`：
   - **CausalOrigin** 仍是 `Agent=A → tool_call=tc-… → Tool=SearchTool`
   - **Authority** 已过期 → 内核 **DENY(`authority_expired`)**
   - 归因边 **仍然记录**：`CausalOrigin → connect(evil.com)`

SearchTool 策略本来会放行这次连接；拒绝的原因是权限窗口已经关上，而不是「不知道是谁干的」。这就是 Causal ≠ Authority。

CodeTool 版本（场景 `codetool_expired_connect`）同样：后台 `connect` 被拒，但 `CausalOrigin.tool_name` 仍是 `CodeTool`。即便 Authority 未过期，CodeTool 策略也禁止公网（场景 `search_vs_code_net`）。

## 和 MtAgentLens 的对比（不是同一类东西）

| | MtAgentLens（proj32） | 本原型 |
|--|----------------------|--------|
| 定位 | 可观测：eBPF 探针 → 责任图 / 污点 / 告警 | 执行范围：**scope 传播** + **策略拒绝** |
| 因果 | 事后从事件还原 ResponsibilityGraph | `CausalOrigin` 跟着执行上下文走 |
| 权限 | 不作为授权门 | `AuthorityScope` 可过期 → DENY |
| 内核 | 真实 eBPF | 仅 `SimulatedKernel` |
| 源码 | 第三方比赛项目，见对比笔记 | 本仓库原创 lab，**未 vendor MtAgentLens** |

MtAgentLens 回答「发生了什么、责任链怎么画」；本设计额外要求内核在 syscall 入口能看见 **ExecutionScopeID**，并区分「归因」与「当前授权」。两者互补，不要混成一个实现。

## 怎么跑

需要 Python 3.11+。pytest 仅测试需要。

```bash
cd research/execution-scope
python3 -m pip install -r requirements.txt
python3 -m pytest -q
python3 -m execution_scope.demo
```

内置场景：

1. **延迟归因** — 工具返回后 `connect(evil.com)` 仍指向 Tool A / tool_call，但 Authority 过期 → DENY
2. **同 PID 复用** — 模拟 pid `4242`：Agent A `connect(github.com)` vs Agent B `unlink(/workspace/tmp/cache)`
3. **CodeTool 过期** — 工具内 `open(/workspace/out.py)` 成功；返回后后台 connect DENY，CausalOrigin 仍是 CodeTool
4. **策略对比** — SearchTool 允许公网；CodeTool 拒绝公网（Authority 仍有效）

## 模块

```text
execution_scope/
  scope.py        ExecutionScopeID, CausalScope, AuthorityScope, ToolPolicy
  context.py      contextvars + thread-local；spawn 时复制 Causal、可选继承/过期 Authority
  runtime.py      agent_run / tool_call / create_task / submit_threadpool /
                  spawn_subprocess（模拟）/ spawn_background
  kernel_sim.py   open/connect/unlink/exec 门：查 Authority 策略，永远记归因边
  provenance.py   由 spawn/syscall 事件建图，还原 Agent→…→syscall
  demo.py         上述四个场景
```

策略摘要：

- **SearchTool**：公网 `connect` 允许；`/etc` 拒绝；工作区写入拒绝
- **CodeTool**：`/workspace` 写入 / unlink 允许；公网 `connect` 拒绝；`exec` 拒绝

`spawn_subprocess` **不会** 真正 `fork`：只分配新的模拟 PID，并把 scope 拷进子执行流。这对应未来「exec 时携带 ExecutionScopeID」的教学模型。

## 未来工作（真实内核）

- 用 eBPF 或 LSM 把 `ExecutionScopeID` 挂到 `task_struct` / cred
- `clone` / `execve` 时传播该 ID（ambient 或显式 scm）
- 在 `security_file_open`、`socket_connect`、`path_unlink`、`bprm_check_security` 查 Authority 并写 Causal 审计
- 用户态 runtime（Agent 框架）在 `tool_call` 进入/退出时更新内核 map：退出即 expire Authority

在那之前，请把本目录只当成可运行的语义规格，而不是安全边界。

## 许可证

MIT。无密钥、无第三方比赛源码。
