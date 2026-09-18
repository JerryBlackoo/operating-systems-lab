import threading

from execution_scope.context import get_bundle, using_bundle
from execution_scope.runtime import AgentRuntime
from execution_scope.scope import SEARCH_TOOL_POLICY, SyscallOp

from execution_scope.demo import catch


def test_contextvar_visible_inside_agent(rt: AgentRuntime) -> None:
    seen: list[str] = []

    def body() -> None:
        bundle = get_bundle()
        assert bundle is not None
        seen.append(bundle.scope_id.agent_id)

    rt.agent_run("AgentZ", body)
    assert seen == ["AgentZ"]
    assert get_bundle() is None


def test_threadpool_copies_causal_origin(rt: AgentRuntime) -> None:
    def tool() -> object:
        fut = rt.submit_threadpool(
            lambda: catch(rt.kernel.connect, "example.com"),
            inherit_authority=True,
            name="tp-net",
        )
        return fut.result(timeout=2)

    event = rt.agent_run(
        "AgentA",
        lambda: rt.tool_call("SearchTool", tool, policy=SEARCH_TOOL_POLICY),
    )
    assert event.allowed
    assert event.causal_origin is not None
    assert event.causal_origin.tool_name == "SearchTool"
    assert event.op is SyscallOp.CONNECT
    assert threading.current_thread() is not None
    assert "tp-net" in event.causal_chain or event.scope_id.task_id == "tp-net"


def test_subprocess_gets_new_pid_same_origin(rt: AgentRuntime) -> None:
    parent_pid: list[int] = []

    def tool() -> object:
        parent_pid.append(get_bundle().pid)  # type: ignore[union-attr]
        proc = rt.spawn_subprocess(
            lambda: catch(rt.kernel.open, "/workspace/README.md"),
            inherit_authority=True,
            name="child-proc",
        )
        event = proc.future.result(timeout=2)
        return proc.pid, event

    child_pid, event = rt.agent_run(
        "AgentA",
        lambda: rt.tool_call("SearchTool", tool, policy=SEARCH_TOOL_POLICY),
    )
    assert child_pid != parent_pid[0]
    assert event.pid == child_pid
    assert event.causal_origin is not None
    assert event.causal_origin.agent_id == "AgentA"
    assert event.causal_origin.tool_name == "SearchTool"
    assert event.allowed


def test_no_inherit_authority_denies_during_live_tool(rt: AgentRuntime) -> None:
    def tool() -> object:
        fut = rt.submit_threadpool(
            lambda: catch(rt.kernel.connect, "example.com"),
            inherit_authority=False,
            name="no-auth",
        )
        return fut.result(timeout=2)

    event = rt.agent_run(
        "AgentA",
        lambda: rt.tool_call("SearchTool", tool, policy=SEARCH_TOOL_POLICY),
    )
    assert not event.allowed
    assert event.deny_reason is not None
    assert event.deny_reason.value == "authority_expired"
    assert event.causal_origin is not None
    assert event.causal_origin.tool_name == "SearchTool"
