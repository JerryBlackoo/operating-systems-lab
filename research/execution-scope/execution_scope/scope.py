"""Logical principals for Agent execution.

Host teaching model only: these types are *not* attached to a real kernel
``task_struct`` / LSM / eBPF map. A future kernel port would carry
``ExecutionScopeID`` across clone/exec and consult it at syscall hooks.

Two axes that must not be collapsed:

* **CausalScope / CausalOrigin** — *who caused this*. Survives tool return,
  thread hops, and simulated subprocesses. Used for attribution.
* **AuthorityScope** — *who is still authorized NOW*. Can expire when the
  tool_call returns, so a leftover async worker cannot keep the tool's
  privileges (async privilege escape).
"""

from __future__ import annotations

import threading
import uuid
from dataclasses import dataclass, field
from enum import Enum
from typing import Mapping


class SyscallOp(str, Enum):
    OPEN = "open"
    CONNECT = "connect"
    UNLINK = "unlink"
    EXEC = "exec"


@dataclass(frozen=True)
class ExecutionScopeID:
    """Structured logical principal, not a PID/TID.

    Readable chain::

        Agent=alice → tool_call=tc-ab12 → Tool=SearchTool → Task=bg-1
    """

    agent_id: str
    tool_call_id: str | None = None
    tool_name: str | None = None
    task_id: str | None = None
    uid: str = field(default_factory=lambda: uuid.uuid4().hex[:8])

    def with_tool_call(self, tool_call_id: str, tool_name: str) -> ExecutionScopeID:
        return ExecutionScopeID(
            agent_id=self.agent_id,
            tool_call_id=tool_call_id,
            tool_name=tool_name,
            task_id=None,
        )

    def with_task(self, task_id: str) -> ExecutionScopeID:
        return ExecutionScopeID(
            agent_id=self.agent_id,
            tool_call_id=self.tool_call_id,
            tool_name=self.tool_name,
            task_id=task_id,
        )

    def chain_labels(self) -> tuple[str, ...]:
        labels = [f"Agent={self.agent_id}"]
        if self.tool_call_id:
            labels.append(f"tool_call={self.tool_call_id}")
        if self.tool_name:
            labels.append(f"Tool={self.tool_name}")
        if self.task_id:
            labels.append(f"Task={self.task_id}")
        return tuple(labels)

    def __str__(self) -> str:
        return " → ".join(self.chain_labels())


@dataclass(frozen=True)
class CausalOrigin:
    """Frozen 'who caused this' snapshot, typically the Agent or tool_call.

    Copied onto every child task/thread/subprocess. Not cleared when the
    originating tool returns — delayed ``connect(evil.com)`` is still
    attributed here even if Authority has already expired.
    """

    agent_id: str
    tool_call_id: str | None
    tool_name: str | None
    origin_scope_id: ExecutionScopeID

    @classmethod
    def from_scope_id(cls, scope_id: ExecutionScopeID) -> CausalOrigin:
        return cls(
            agent_id=scope_id.agent_id,
            tool_call_id=scope_id.tool_call_id,
            tool_name=scope_id.tool_name,
            origin_scope_id=scope_id,
        )

    def __str__(self) -> str:
        return str(self.origin_scope_id)


@dataclass(frozen=True)
class CausalFrame:
    """One hop in the provenance chain (Agent / tool_call / Tool / Task / …)."""

    kind: str
    name: str
    scope_id: ExecutionScopeID

    def label(self) -> str:
        return f"{self.kind}={self.name}"


@dataclass(frozen=True)
class CausalScope:
    """Causal origin plus the spawn chain that reached the current frame."""

    origin: CausalOrigin
    frames: tuple[CausalFrame, ...] = ()

    def extend(self, frame: CausalFrame) -> CausalScope:
        # Origin is copied, never rewritten by later tasks.
        return CausalScope(origin=self.origin, frames=self.frames + (frame,))

    def chain(self) -> str:
        return " → ".join(frame.label() for frame in self.frames)


@dataclass(frozen=True)
class ToolPolicy:
    """Capability envelope attached to a live AuthorityScope.

    Defaults encode the lab examples:

    * SearchTool: public net yes, ``/etc`` no, workspace write no
    * CodeTool: workspace write yes, public net no
    """

    name: str
    allow_public_net: bool = False
    allowed_hosts: frozenset[str] = field(default_factory=frozenset)
    allow_workspace_write: bool = False
    workspace_root: str = "/workspace"
    deny_path_prefixes: tuple[str, ...] = ("/etc",)
    allow_unlink: bool = False
    allow_exec: bool = False
    allow_open_read: bool = True

    def in_workspace(self, path: str) -> bool:
        root = self.workspace_root.rstrip("/") or "/"
        if root == "/":
            return True
        return path == root or path.startswith(root + "/")

    def path_denied(self, path: str) -> bool:
        for prefix in self.deny_path_prefixes:
            p = prefix.rstrip("/") or "/"
            if p == "/":
                return True
            if path == p or path.startswith(p + "/"):
                return True
        return False

    def allows(self, op: SyscallOp | str, resource: str, *, write: bool = False) -> bool:
        op = SyscallOp(op)
        if op is SyscallOp.CONNECT:
            host = resource.split(":", 1)[0].lower()
            allowed = {h.lower() for h in self.allowed_hosts}
            if host in allowed:
                return True
            return self.allow_public_net
        if op is SyscallOp.OPEN:
            if self.path_denied(resource):
                return False
            if write:
                return self.allow_workspace_write and self.in_workspace(resource)
            return self.allow_open_read
        if op is SyscallOp.UNLINK:
            if self.path_denied(resource):
                return False
            if not self.in_workspace(resource):
                return False
            return self.allow_unlink or self.allow_workspace_write
        if op is SyscallOp.EXEC:
            return self.allow_exec
        return False


SEARCH_TOOL_POLICY = ToolPolicy(
    name="SearchTool",
    allow_public_net=True,
    allow_workspace_write=False,
    deny_path_prefixes=("/etc",),
)

CODE_TOOL_POLICY = ToolPolicy(
    name="CodeTool",
    allow_public_net=False,
    allow_workspace_write=True,
    deny_path_prefixes=("/etc",),
)

AGENT_BASE_POLICY = ToolPolicy(
    name="AgentBase",
    allow_public_net=False,
    allow_workspace_write=False,
    allow_open_read=True,
)

BUILTIN_POLICIES: Mapping[str, ToolPolicy] = {
    "SearchTool": SEARCH_TOOL_POLICY,
    "CodeTool": CODE_TOOL_POLICY,
    "AgentBase": AGENT_BASE_POLICY,
}


def builtin_policy(tool_name: str) -> ToolPolicy:
    if tool_name in BUILTIN_POLICIES:
        return BUILTIN_POLICIES[tool_name]
    return ToolPolicy(name=tool_name)


class AuthorityScope:
    """Live authorization for the current logical principal.

    ``inherit()`` returns *this same object* so ``expire()`` is visible to
    every async/thread/subprocess child that opted in. That is the fence
    against stale privilege: tool returns → expire → leftover workers DENY
    even though CausalOrigin still names the tool.
    """

    def __init__(
        self,
        principal: ExecutionScopeID,
        policy: ToolPolicy,
        *,
        expired: bool = False,
    ) -> None:
        self.principal = principal
        self.policy = policy
        self._expired = expired
        self._lock = threading.RLock()

    def inherit(self) -> AuthorityScope:
        return self

    def expired_view(self) -> AuthorityScope:
        """New object: same policy metadata, already unauthorized."""
        return AuthorityScope(self.principal, self.policy, expired=True)

    def expire(self) -> None:
        with self._lock:
            self._expired = True

    @property
    def expired(self) -> bool:
        with self._lock:
            return self._expired

    def is_active(self) -> bool:
        return not self.expired

    def __repr__(self) -> str:
        state = "expired" if self.expired else "active"
        return f"AuthorityScope({self.principal}, policy={self.policy.name}, {state})"
