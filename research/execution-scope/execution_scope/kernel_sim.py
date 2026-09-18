"""Simulated syscall gate: policy on AuthorityScope, attribution on CausalOrigin.

This is **not** a real LSM or eBPF hook. Every ``open`` / ``connect`` /
``unlink`` / ``exec`` here is a host-side check that:

1. always records ``CausalOrigin → syscall`` (even on DENY);
2. DENY when no scope is installed, authority is expired, or policy forbids.

No actual files, sockets, or processes are touched.
"""

from __future__ import annotations

import threading
import time
import uuid
from dataclasses import dataclass, field
from enum import Enum
from typing import Callable, Sequence

from .context import get_bundle
from .scope import CausalOrigin, ExecutionScopeID, SyscallOp


class DenyReason(str, Enum):
    NO_SCOPE = "no_execution_scope"
    AUTHORITY_EXPIRED = "authority_expired"
    POLICY_FORBIDDEN = "policy_forbidden"


@dataclass
class SyscallEvent:
    event_id: str
    op: SyscallOp
    resource: str
    write: bool
    pid: int
    tid: int
    causal_origin: CausalOrigin | None
    causal_chain: str
    scope_id: ExecutionScopeID | None
    policy_name: str | None
    authority_active: bool
    policy_would_allow: bool | None
    allowed: bool
    deny_reason: DenyReason | None
    ts: float = field(default_factory=time.time)

    def summary(self) -> str:
        decision = "ALLOW" if self.allowed else f"DENY({self.deny_reason.value if self.deny_reason else '?'})"
        origin = str(self.causal_origin) if self.causal_origin else "<none>"
        return (
            f"{self.op.value}({self.resource}) {decision} "
            f"pid={self.pid} tid={self.tid} "
            f"origin=[{origin}]"
        )


class SyscallDenied(Exception):
    def __init__(self, event: SyscallEvent) -> None:
        self.event = event
        super().__init__(event.summary())


def _mode_writes(mode: str) -> bool:
    return any(flag in mode for flag in ("w", "a", "x", "+"))


class SimulatedKernel:
    """In-process stand-in for a kernel enforcement + audit point."""

    def __init__(self) -> None:
        self.events: list[SyscallEvent] = []
        self._pid_seq = 1000
        self._tid_seq = 1
        self._lock = threading.Lock()
        self.on_event: Callable[[SyscallEvent], None] | None = None

    def alloc_pid(self) -> int:
        with self._lock:
            self._pid_seq += 1
            return self._pid_seq

    def alloc_tid(self) -> int:
        with self._lock:
            self._tid_seq += 1
            return self._tid_seq

    def _emit(self, event: SyscallEvent) -> SyscallEvent:
        with self._lock:
            self.events.append(event)
        if self.on_event is not None:
            self.on_event(event)
        if not event.allowed:
            raise SyscallDenied(event)
        return event

    def _gate(self, op: SyscallOp, resource: str, *, write: bool = False) -> SyscallEvent:
        bundle = get_bundle()
        event_id = uuid.uuid4().hex[:12]
        if bundle is None:
            return self._emit(
                SyscallEvent(
                    event_id=event_id,
                    op=op,
                    resource=resource,
                    write=write,
                    pid=-1,
                    tid=-1,
                    causal_origin=None,
                    causal_chain="",
                    scope_id=None,
                    policy_name=None,
                    authority_active=False,
                    policy_would_allow=None,
                    allowed=False,
                    deny_reason=DenyReason.NO_SCOPE,
                )
            )

        policy = bundle.authority.policy
        policy_would_allow = policy.allows(op, resource, write=write)
        active = bundle.authority.is_active()
        deny: DenyReason | None
        allowed: bool
        if not active:
            deny = DenyReason.AUTHORITY_EXPIRED
            allowed = False
        elif not policy_would_allow:
            deny = DenyReason.POLICY_FORBIDDEN
            allowed = False
        else:
            deny = None
            allowed = True

        return self._emit(
            SyscallEvent(
                event_id=event_id,
                op=op,
                resource=resource,
                write=write,
                pid=bundle.pid,
                tid=bundle.tid,
                causal_origin=bundle.causal.origin,
                causal_chain=bundle.causal.chain(),
                scope_id=bundle.scope_id,
                policy_name=policy.name,
                authority_active=active,
                policy_would_allow=policy_would_allow,
                allowed=allowed,
                deny_reason=deny,
            )
        )

    def open(self, path: str, mode: str = "r") -> SyscallEvent:
        return self._gate(SyscallOp.OPEN, path, write=_mode_writes(mode))

    def connect(self, host: str, port: int = 443) -> SyscallEvent:
        return self._gate(SyscallOp.CONNECT, f"{host}:{port}")

    def unlink(self, path: str) -> SyscallEvent:
        return self._gate(SyscallOp.UNLINK, path)

    def execve(self, path: str, argv: Sequence[str] | None = None) -> SyscallEvent:
        resource = path if not argv else f"{path} {' '.join(argv)}"
        return self._gate(SyscallOp.EXEC, resource)
