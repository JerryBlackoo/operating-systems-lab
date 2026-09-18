import asyncio

from execution_scope.demo import catch
from execution_scope.kernel_sim import DenyReason
from execution_scope.runtime import AgentRuntime
from execution_scope.scope import SEARCH_TOOL_POLICY, SyscallOp


def test_asyncio_task_propagates_causal_and_authority() -> None:
    async def runner() -> None:
        with AgentRuntime() as rt:
            async def work():
                return catch(rt.kernel.connect, "example.com")

            def tool():
                task = rt.create_task(work(), inherit_authority=True, name="aio-net")
                return task

            task = rt.agent_run(
                "AgentA",
                lambda: rt.tool_call("SearchTool", tool, policy=SEARCH_TOOL_POLICY),
            )
            event = await task
            assert event.allowed
            assert event.causal_origin is not None
            assert event.causal_origin.tool_name == "SearchTool"
            assert event.scope_id is not None
            assert event.scope_id.task_id == "aio-net"

    asyncio.run(runner())


def test_asyncio_after_tool_return_expires_authority() -> None:
    async def runner() -> None:
        with AgentRuntime() as rt:
            async def work():
                return catch(rt.kernel.connect, "evil.com")

            def tool():
                return rt.create_task(
                    work(),
                    inherit_authority=True,
                    after_tool_return=True,
                    name="aio-late",
                )

            task = rt.agent_run(
                "AgentA",
                lambda: rt.tool_call("SearchTool", tool, policy=SEARCH_TOOL_POLICY),
            )
            event = await task
            assert not event.allowed
            assert event.deny_reason is DenyReason.AUTHORITY_EXPIRED
            assert event.causal_origin is not None
            assert event.causal_origin.tool_name == "SearchTool"
            assert event.policy_would_allow is True
            assert event.op is SyscallOp.CONNECT

    asyncio.run(runner())
