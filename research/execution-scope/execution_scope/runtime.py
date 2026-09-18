"""Simulated Agent runtime: tool_call, asyncio tasks, threads, subprocesses.

Scope rules on every spawn:

* copy CausalOrigin (attribution survives tool return)
* optionally inherit Authority (shared expire) or install an expired view

``spawn_subprocess`` does **not** fork a real OS process. It allocates a
new simulated PID and runs the child with a copied scope — the teaching
analogue of "pass ExecutionScopeID across exec".
"""

from __future__ import annotations

import asyncio
import inspect
import threading
import uuid
from concurrent.futures import Future, ThreadPoolExecutor
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Any, Awaitable, Callable, Coroutine, Iterator, TypeVar

from .context import ScopeBundle, copy_for_spawn, require_bundle, using_bundle
from .kernel_sim import SimulatedKernel, SyscallEvent
from .provenance import ProvenanceGraph
from .scope import (
    AGENT_BASE_POLICY,
    AuthorityScope,
    CausalFrame,
    CausalOrigin,
    CausalScope,
    ExecutionScopeID,
    ToolPolicy,
    builtin_policy,
)

T = TypeVar("T")


@dataclass
class ToolCallFrame:
    tool_call_id: str
    authority: AuthorityScope
    returned: threading.Event
    bundle: ScopeBundle


@dataclass
class SimulatedProcess:
    pid: int
    future: Future[Any]
    task_id: str


class AgentRuntime:
    def __init__(self, kernel: SimulatedKernel | None = None) -> None:
        self.kernel = kernel or SimulatedKernel()
        self.graph = ProvenanceGraph()
        self.kernel.on_event = self._on_syscall
        self._pool = ThreadPoolExecutor(max_workers=8, thread_name_prefix="es-worker")
        self._bg_threads: list[threading.Thread] = []
        self._tool_stack: list[ToolCallFrame] = []
        self._lock = threading.Lock()

    def close(self) -> None:
        self._pool.shutdown(wait=False, cancel_futures=True)

    def __enter__(self) -> AgentRuntime:
        return self

    def __exit__(self, *exc: object) -> None:
        self.drain(timeout=1.0)
        self.close()

    def _on_syscall(self, event: SyscallEvent) -> None:
        self.graph.record_syscall(event, require_bundle() if event.scope_id is not None else None)

    def _current_tool(self) -> ToolCallFrame | None:
        with self._lock:
            return self._tool_stack[-1] if self._tool_stack else None

    @contextmanager
    def _agent_scope(self, agent_id: str, *, pid: int | None = None) -> Iterator[ScopeBundle]:
        scope_id = ExecutionScopeID(agent_id=agent_id)
        origin = CausalOrigin.from_scope_id(scope_id)
        frames = (CausalFrame(kind="agent", name=agent_id, scope_id=scope_id),)
        causal = CausalScope(origin=origin, frames=frames)
        authority = AuthorityScope(scope_id, AGENT_BASE_POLICY)
        bundle = ScopeBundle(
            scope_id=scope_id,
            causal=causal,
            authority=authority,
            pid=self.kernel.alloc_pid() if pid is None else pid,
            tid=self.kernel.alloc_tid(),
        )
        self.graph.add_bundle(bundle, kind="agent")
        with using_bundle(bundle):
            yield bundle

    @contextmanager
    def _tool_scope(
        self,
        tool_name: str,
        *,
        policy: ToolPolicy | None = None,
        expire_on_return: bool = True,
        tool_call_id: str | None = None,
    ) -> Iterator[ScopeBundle]:
        parent = require_bundle()
        tool_call_id = tool_call_id or f"tc-{uuid.uuid4().hex[:8]}"
        scope_id = parent.scope_id.with_tool_call(tool_call_id, tool_name)
        policy = policy or builtin_policy(tool_name)
        origin = CausalOrigin.from_scope_id(scope_id)
        frames = parent.causal.frames + (
            CausalFrame(kind="tool_call", name=tool_call_id, scope_id=scope_id),
            CausalFrame(kind="tool", name=tool_name, scope_id=scope_id),
        )
        causal = CausalScope(origin=origin, frames=frames)
        authority = AuthorityScope(scope_id, policy)
        bundle = ScopeBundle(
            scope_id=scope_id,
            causal=causal,
            authority=authority,
            pid=parent.pid,
            tid=parent.tid,
        )
        self.graph.add_spawn(parent, bundle, kind="tool_call")
        returned = threading.Event()
        frame = ToolCallFrame(tool_call_id, authority, returned, bundle)
        with using_bundle(bundle):
            with self._lock:
                self._tool_stack.append(frame)
            try:
                yield bundle
            finally:
                if expire_on_return:
                    authority.expire()
                returned.set()
                with self._lock:
                    if self._tool_stack and self._tool_stack[-1] is frame:
                        self._tool_stack.pop()

    def agent_run(self, agent_id: str, fn: Callable[[], T], *, pid: int | None = None) -> T:
        with self._agent_scope(agent_id, pid=pid):
            return fn()

    async def agent_run_async(
        self,
        agent_id: str,
        fn: Callable[[], T | Awaitable[T]],
        *,
        pid: int | None = None,
    ) -> T:
        with self._agent_scope(agent_id, pid=pid):
            return await _maybe_await(fn())

    def tool_call(
        self,
        tool_name: str,
        fn: Callable[[], T],
        *,
        policy: ToolPolicy | None = None,
        expire_on_return: bool = True,
        tool_call_id: str | None = None,
    ) -> T:
        with self._tool_scope(
            tool_name,
            policy=policy,
            expire_on_return=expire_on_return,
            tool_call_id=tool_call_id,
        ):
            return fn()

    async def tool_call_async(
        self,
        tool_name: str,
        fn: Callable[[], T | Awaitable[T]],
        *,
        policy: ToolPolicy | None = None,
        expire_on_return: bool = True,
        tool_call_id: str | None = None,
    ) -> T:
        """Like ``tool_call``, but Authority stays live across ``await`` inside the tool."""
        with self._tool_scope(
            tool_name,
            policy=policy,
            expire_on_return=expire_on_return,
            tool_call_id=tool_call_id,
        ):
            return await _maybe_await(fn())

    def _child_bundle(
        self,
        *,
        inherit_authority: bool,
        name: str | None,
        kind: str,
        new_pid: int | None = None,
        new_tid: int | None = None,
    ) -> tuple[str, ScopeBundle]:
        task_id = name or f"{kind}-{uuid.uuid4().hex[:6]}"
        parent = require_bundle()
        child = copy_for_spawn(
            task_id,
            inherit_authority=inherit_authority,
            kind=kind,
            new_pid=new_pid,
            new_tid=new_tid,
        )
        self.graph.add_spawn(parent, child, kind=kind)
        return task_id, child

    def create_task(
        self,
        coro: Coroutine[Any, Any, T],
        *,
        inherit_authority: bool = True,
        after_tool_return: bool = False,
        name: str | None = None,
    ) -> asyncio.Task[T]:
        """asyncio.create_task analogue that installs a child ScopeBundle."""
        tool = self._current_tool()
        task_id, child = self._child_bundle(
            inherit_authority=inherit_authority,
            name=name,
            kind="task",
            new_tid=self.kernel.alloc_tid(),
        )

        async def wrapped() -> T:
            if after_tool_return and tool is not None:
                await asyncio.to_thread(tool.returned.wait)
            with using_bundle(child):
                return await coro

        return asyncio.create_task(wrapped(), name=task_id)

    def submit_threadpool(
        self,
        fn: Callable[[], T],
        *,
        inherit_authority: bool = True,
        after_tool_return: bool = False,
        name: str | None = None,
    ) -> Future[T]:
        tool = self._current_tool()
        _task_id, child = self._child_bundle(
            inherit_authority=inherit_authority,
            name=name,
            kind="thread",
            new_tid=self.kernel.alloc_tid(),
        )

        def wrapped() -> T:
            if after_tool_return and tool is not None:
                tool.returned.wait()
            with using_bundle(child):
                return fn()

        return self._pool.submit(wrapped)

    def spawn_subprocess(
        self,
        fn: Callable[[], T],
        *,
        inherit_authority: bool = True,
        after_tool_return: bool = False,
        name: str | None = None,
    ) -> SimulatedProcess:
        """Simulated subprocess: new PID, copied CausalOrigin."""
        tool = self._current_tool()
        new_pid = self.kernel.alloc_pid()
        task_id, child = self._child_bundle(
            inherit_authority=inherit_authority,
            name=name,
            kind="subprocess",
            new_pid=new_pid,
            new_tid=1,
        )

        def wrapped() -> T:
            if after_tool_return and tool is not None:
                tool.returned.wait()
            with using_bundle(child):
                return fn()

        future = self._pool.submit(wrapped)
        return SimulatedProcess(pid=new_pid, future=future, task_id=task_id)

    def spawn_background(
        self,
        fn: Callable[[], T],
        *,
        inherit_authority: bool = True,
        after_tool_return: bool = False,
        name: str | None = None,
    ) -> threading.Thread:
        """Fire-and-forget worker; optionally wait until the current tool returns."""
        tool = self._current_tool()
        task_id, child = self._child_bundle(
            inherit_authority=inherit_authority,
            name=name,
            kind="task",
            new_tid=self.kernel.alloc_tid(),
        )
        done: list[T] = []

        def wrapped() -> None:
            if after_tool_return and tool is not None:
                tool.returned.wait()
            with using_bundle(child):
                done.append(fn())

        thread = threading.Thread(target=wrapped, name=task_id, daemon=True)
        thread.start()
        with self._lock:
            self._bg_threads.append(thread)
        return thread

    def drain(self, timeout: float = 2.0) -> None:
        with self._lock:
            threads = list(self._bg_threads)
        for thread in threads:
            thread.join(timeout=timeout)


async def _maybe_await(result: T | Awaitable[T]) -> T:
    if inspect.isawaitable(result):
        return await result
    return result
