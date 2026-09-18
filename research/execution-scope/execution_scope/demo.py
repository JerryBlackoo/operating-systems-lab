"""Runnable teaching scenarios for ExecutionScope.

Each scenario returns structured events so tests and ``python -m
execution_scope.demo`` share the same paths.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Callable

from .kernel_sim import SyscallDenied, SyscallEvent
from .runtime import AgentRuntime
from .scope import CODE_TOOL_POLICY, SEARCH_TOOL_POLICY, SyscallOp

SHARED_WORKER_PID = 4242


def catch(fn: Callable[..., SyscallEvent], *args: object, **kwargs: object) -> SyscallEvent:
    try:
        return fn(*args, **kwargs)
    except SyscallDenied as exc:
        return exc.event


@dataclass
class ScenarioReport:
    name: str
    title: str
    events: list[SyscallEvent] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)


def scenario_delayed_attribution(rt: AgentRuntime) -> ScenarioReport:
    """Tool A returns; background connect(evil.com) is still attributed to A.

    SearchTool *policy* would allow public net, but Authority expires on
    return, so the delayed connect is DENY(authority_expired) while
    CausalOrigin still names SearchTool / tool_call.
    """

    def tool_body() -> str:
        rt.spawn_background(
            lambda: catch(rt.kernel.connect, "evil.com"),
            inherit_authority=True,
            after_tool_return=True,
            name="bg-connect-evil",
        )
        return "search-done"

    rt.agent_run("AgentA", lambda: rt.tool_call("SearchTool", tool_body, policy=SEARCH_TOOL_POLICY))
    rt.drain()
    events = list(rt.kernel.events)
    origin = events[-1].causal_origin if events else None
    return ScenarioReport(
        name="delayed_attribution",
        title="工具返回后的延迟副作用：归因仍指向 Tool A / tool_call",
        events=events,
        notes=[
            "SearchTool 已返回，AuthorityScope 已过期。",
            f"CausalOrigin 仍是 {origin}。" if origin else "没有记录到 syscall。",
            "内核仍写下 CausalOrigin → connect(evil.com)；判定为 DENY(authority_expired)。",
            "这就是 Causal ≠ Authority：谁造成的 vs 现在还能否做。",
        ],
    )


def scenario_pid_multiplex(rt: AgentRuntime) -> ScenarioReport:
    """Same simulated PID serves Agent A connect(github) and Agent B unlink."""

    def agent_a() -> SyscallEvent:
        return rt.tool_call(
            "SearchTool",
            lambda: catch(rt.kernel.connect, "github.com"),
            policy=SEARCH_TOOL_POLICY,
        )

    def agent_b() -> SyscallEvent:
        return rt.tool_call(
            "CodeTool",
            lambda: catch(rt.kernel.unlink, "/workspace/tmp/cache"),
            policy=CODE_TOOL_POLICY,
        )

    ev_a = rt.agent_run("AgentA", agent_a, pid=SHARED_WORKER_PID)
    ev_b = rt.agent_run("AgentB", agent_b, pid=SHARED_WORKER_PID)
    return ScenarioReport(
        name="pid_multiplex",
        title="同一 PID 复用：Agent A connect vs Agent B unlink 归因不串台",
        events=[ev_a, ev_b],
        notes=[
            f"两个 syscall 的 pid 都是 {SHARED_WORKER_PID}（模拟工作线程池复用进程）。",
            f"connect 归因 Agent={ev_a.causal_origin.agent_id if ev_a.causal_origin else '?'} Tool={ev_a.causal_origin.tool_name if ev_a.causal_origin else '?'}.",
            f"unlink 归因 Agent={ev_b.causal_origin.agent_id if ev_b.causal_origin else '?'} Tool={ev_b.causal_origin.tool_name if ev_b.causal_origin else '?'}.",
            "只看 PID/TID 无法区分；必须看 ExecutionScopeID。",
        ],
    )


def scenario_codetool_expired_connect(rt: AgentRuntime) -> ScenarioReport:
    """CodeTool workspace write succeeds; delayed public connect is DENY expired."""

    live: list[SyscallEvent] = []

    def tool_body() -> SyscallEvent:
        live.append(catch(rt.kernel.open, "/workspace/out.py", "w"))
        rt.spawn_background(
            lambda: catch(rt.kernel.connect, "evil.com"),
            inherit_authority=True,
            after_tool_return=True,
            name="bg-code-connect",
        )
        return live[0]

    rt.agent_run(
        "AgentCoder",
        lambda: rt.tool_call("CodeTool", tool_body, policy=CODE_TOOL_POLICY),
    )
    rt.drain()
    events = list(rt.kernel.events)
    delayed = next(ev for ev in events if ev.op is SyscallOp.CONNECT)
    return ScenarioReport(
        name="codetool_expired_connect",
        title="CodeTool 返回后后台 connect：Authority 过期，CausalOrigin 仍是 CodeTool",
        events=events,
        notes=[
            f"工具仍在执行时 open(/workspace/out.py) → {'ALLOW' if live and live[0].allowed else 'DENY'}。",
            f"返回后 connect(evil.com) → DENY({delayed.deny_reason.value if delayed.deny_reason else '?'})。",
            f"CausalOrigin.tool_name = {delayed.causal_origin.tool_name if delayed.causal_origin else None}。",
            f"authority_active = {delayed.authority_active}；policy_would_allow = {delayed.policy_would_allow}。",
        ],
    )


def scenario_search_allow_code_deny_net(rt: AgentRuntime) -> ScenarioReport:
    """Live authority: SearchTool may connect; CodeTool may not."""

    search = rt.agent_run(
        "AgentA",
        lambda: rt.tool_call(
            "SearchTool",
            lambda: catch(rt.kernel.connect, "api.example.com"),
            policy=SEARCH_TOOL_POLICY,
        ),
    )
    code = rt.agent_run(
        "AgentB",
        lambda: rt.tool_call(
            "CodeTool",
            lambda: catch(rt.kernel.connect, "api.example.com"),
            policy=CODE_TOOL_POLICY,
        ),
    )
    return ScenarioReport(
        name="search_vs_code_net",
        title="策略对比：SearchTool 允许公网 connect；CodeTool 拒绝",
        events=[search, code],
        notes=[
            f"SearchTool connect(api.example.com) → {'ALLOW' if search.allowed else 'DENY'}。",
            f"CodeTool connect(api.example.com) → DENY({code.deny_reason.value if code.deny_reason else '?'})。",
            "两条调用都发生在工具仍持有 Authority 的窗口内，因此这是策略差异而不是过期。",
        ],
    )


SCENARIOS: tuple[Callable[[AgentRuntime], ScenarioReport], ...] = (
    scenario_delayed_attribution,
    scenario_pid_multiplex,
    scenario_codetool_expired_connect,
    scenario_search_allow_code_deny_net,
)


def format_report(report: ScenarioReport, rt: AgentRuntime) -> str:
    lines = [
        "",
        "=" * 72,
        f"场景 {report.name}",
        report.title,
        "-" * 72,
    ]
    for i, event in enumerate(report.events, 1):
        lines.append(f"  [{i}] {event.summary()}")
        if event.causal_chain:
            lines.append(f"      chain: {event.causal_chain}")
        chain = rt.graph.format_chain(event)
        lines.append(f"      provenance: {chain}")
    lines.append("说明:")
    for note in report.notes:
        lines.append(f"  - {note}")
    return "\n".join(lines)


def main() -> int:
    print("ExecutionScope 宿主教学原型（非真实 LSM/eBPF）")
    print("CausalOrigin 负责归因；AuthorityScope 负责现在能不能做。")
    for runner in SCENARIOS:
        with AgentRuntime() as rt:
            report = runner(rt)
            print(format_report(report, rt))
    print()
    print("全部场景已跑完。测试：python3 -m pytest -q")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
