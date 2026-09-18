from execution_scope.demo import (
    SHARED_WORKER_PID,
    scenario_codetool_expired_connect,
    scenario_delayed_attribution,
    scenario_pid_multiplex,
    scenario_search_allow_code_deny_net,
)
from execution_scope.kernel_sim import DenyReason
from execution_scope.runtime import AgentRuntime
from execution_scope.scope import SyscallOp


def test_delayed_connect_attributed_to_tool_a(rt: AgentRuntime) -> None:
    report = scenario_delayed_attribution(rt)
    assert len(report.events) == 1
    event = report.events[0]
    assert event.op is SyscallOp.CONNECT
    assert "evil.com" in event.resource
    assert event.causal_origin is not None
    assert event.causal_origin.agent_id == "AgentA"
    assert event.causal_origin.tool_name == "SearchTool"
    assert event.causal_origin.tool_call_id
    assert not event.allowed
    assert event.deny_reason is DenyReason.AUTHORITY_EXPIRED
    assert event.policy_would_allow is True
    assert "bg-connect-evil" in event.causal_chain


def test_same_pid_multiplex_attribution(rt: AgentRuntime) -> None:
    report = scenario_pid_multiplex(rt)
    connect, unlink = report.events
    assert connect.pid == unlink.pid == SHARED_WORKER_PID
    assert connect.allowed
    assert unlink.allowed
    assert connect.causal_origin is not None
    assert unlink.causal_origin is not None
    assert connect.causal_origin.agent_id == "AgentA"
    assert connect.causal_origin.tool_name == "SearchTool"
    assert unlink.causal_origin.agent_id == "AgentB"
    assert unlink.causal_origin.tool_name == "CodeTool"
    assert connect.op is SyscallOp.CONNECT
    assert "github.com" in connect.resource
    assert unlink.op is SyscallOp.UNLINK


def test_codetool_background_connect_deny_origin_persists(rt: AgentRuntime) -> None:
    report = scenario_codetool_expired_connect(rt)
    opens = [e for e in report.events if e.op is SyscallOp.OPEN]
    connects = [e for e in report.events if e.op is SyscallOp.CONNECT]
    assert len(opens) == 1 and opens[0].allowed
    assert opens[0].causal_origin is not None
    assert opens[0].causal_origin.tool_name == "CodeTool"
    assert len(connects) == 1
    delayed = connects[0]
    assert not delayed.allowed
    assert delayed.deny_reason is DenyReason.AUTHORITY_EXPIRED
    assert delayed.causal_origin is not None
    assert delayed.causal_origin.tool_name == "CodeTool"
    assert delayed.authority_active is False
    assert delayed.policy_would_allow is False


def test_search_allow_code_deny_public_connect(rt: AgentRuntime) -> None:
    report = scenario_search_allow_code_deny_net(rt)
    search, code = report.events
    assert search.allowed
    assert search.causal_origin is not None
    assert search.causal_origin.tool_name == "SearchTool"
    assert not code.allowed
    assert code.deny_reason is DenyReason.POLICY_FORBIDDEN
    assert code.causal_origin is not None
    assert code.causal_origin.tool_name == "CodeTool"
    assert code.authority_active is True
