import pytest

from execution_scope.demo import catch
from execution_scope.kernel_sim import DenyReason, SimulatedKernel, SyscallDenied
from execution_scope.runtime import AgentRuntime
from execution_scope.scope import CODE_TOOL_POLICY, SEARCH_TOOL_POLICY, SyscallOp


def test_syscall_without_scope_is_denied() -> None:
    kernel = SimulatedKernel()
    with pytest.raises(SyscallDenied) as exc:
        kernel.connect("example.com")
    assert exc.value.event.deny_reason is DenyReason.NO_SCOPE
    assert kernel.events[0].causal_origin is None


def test_search_open_etc_denied_while_live(rt: AgentRuntime) -> None:
    event = rt.agent_run(
        "AgentA",
        lambda: rt.tool_call(
            "SearchTool",
            lambda: catch(rt.kernel.open, "/etc/passwd"),
            policy=SEARCH_TOOL_POLICY,
        ),
    )
    assert not event.allowed
    assert event.deny_reason is DenyReason.POLICY_FORBIDDEN
    assert event.causal_origin is not None
    assert event.causal_origin.tool_name == "SearchTool"


def test_code_tool_exec_denied(rt: AgentRuntime) -> None:
    event = rt.agent_run(
        "AgentA",
        lambda: rt.tool_call(
            "CodeTool",
            lambda: catch(rt.kernel.execve, "/bin/bash", ["-c", "id"]),
            policy=CODE_TOOL_POLICY,
        ),
    )
    assert not event.allowed
    assert event.deny_reason is DenyReason.POLICY_FORBIDDEN
    assert event.op is SyscallOp.EXEC


def test_deny_still_records_attribution_edge(rt: AgentRuntime) -> None:
    event = rt.agent_run(
        "AgentA",
        lambda: rt.tool_call(
            "CodeTool",
            lambda: catch(rt.kernel.connect, "evil.com"),
            policy=CODE_TOOL_POLICY,
        ),
    )
    assert not event.allowed
    assert len(rt.kernel.events) == 1
    chain = rt.graph.chain_labels(event)
    assert any("Agent=AgentA" in label for label in chain)
    assert any("syscall" in node.kind for node in rt.graph.reconstruct(event))
