from execution_scope.demo import catch, scenario_delayed_attribution
from execution_scope.runtime import AgentRuntime
from execution_scope.scope import SEARCH_TOOL_POLICY


def test_reconstruct_chain_agent_tool_task_syscall(rt: AgentRuntime) -> None:
    def tool() -> object:
        fut = rt.submit_threadpool(
            lambda: catch(rt.kernel.connect, "example.com"),
            inherit_authority=True,
            name="worker-9",
        )
        return fut.result(timeout=2)

    event = rt.agent_run(
        "AgentA",
        lambda: rt.tool_call("SearchTool", tool, policy=SEARCH_TOOL_POLICY, tool_call_id="tc-fixed"),
    )
    nodes = rt.graph.reconstruct(event)
    kinds = [n.kind for n in nodes]
    assert kinds[0] == "agent"
    assert "tool_call" in kinds
    assert "thread" in kinds or "task" in kinds
    assert kinds[-1] == "syscall"
    labels = " | ".join(n.label for n in nodes)
    assert "Agent=AgentA" in labels
    assert "SearchTool" in labels
    assert "connect" in labels


def test_delayed_scenario_provenance_names_search_tool(rt: AgentRuntime) -> None:
    report = scenario_delayed_attribution(rt)
    event = report.events[0]
    chain = rt.graph.format_chain(event)
    assert "Agent=AgentA" in chain
    assert "SearchTool" in chain
    assert "evil.com" in chain
    assert "DENY" in chain
