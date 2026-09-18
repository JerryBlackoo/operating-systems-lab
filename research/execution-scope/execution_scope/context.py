"""contextvars + thread-local propagation of the current ExecutionScope.

Spawn helpers copy **CausalOrigin** always, and either share Authority
(``inherit_authority=True``) or hand the child an already-expired view.
"""

from __future__ import annotations

import contextvars
import threading
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Iterator

from .scope import (
    AuthorityScope,
    CausalFrame,
    CausalScope,
    ExecutionScopeID,
)


_bundle: contextvars.ContextVar["ScopeBundle | None"] = contextvars.ContextVar(
    "execution_scope_bundle",
    default=None,
)
_tls = threading.local()


@dataclass
class ScopeBundle:
    """Current logical principal as seen by the simulated kernel."""

    scope_id: ExecutionScopeID
    causal: CausalScope
    authority: AuthorityScope
    pid: int
    tid: int

    def child(
        self,
        task_id: str,
        *,
        inherit_authority: bool,
        kind: str = "task",
        new_pid: int | None = None,
        new_tid: int | None = None,
    ) -> ScopeBundle:
        new_id = self.scope_id.with_task(task_id)
        frame = CausalFrame(kind=kind, name=task_id, scope_id=new_id)
        authority = (
            self.authority.inherit()
            if inherit_authority
            else self.authority.expired_view()
        )
        return ScopeBundle(
            scope_id=new_id,
            causal=self.causal.extend(frame),
            authority=authority,
            pid=self.pid if new_pid is None else new_pid,
            tid=self.tid if new_tid is None else new_tid,
        )


def get_bundle() -> ScopeBundle | None:
    bundle = _bundle.get()
    if bundle is not None:
        return bundle
    return getattr(_tls, "bundle", None)


def require_bundle() -> ScopeBundle:
    bundle = get_bundle()
    if bundle is None:
        raise RuntimeError("no ExecutionScope is installed on this task/thread")
    return bundle


def set_bundle(bundle: ScopeBundle | None) -> contextvars.Token:
    _tls.bundle = bundle
    return _bundle.set(bundle)


def reset_bundle(token: contextvars.Token) -> None:
    _bundle.reset(token)
    _tls.bundle = _bundle.get()


@contextmanager
def using_bundle(bundle: ScopeBundle) -> Iterator[ScopeBundle]:
    token = set_bundle(bundle)
    try:
        yield bundle
    finally:
        reset_bundle(token)


def copy_for_spawn(
    task_id: str,
    *,
    inherit_authority: bool = True,
    kind: str = "task",
    new_pid: int | None = None,
    new_tid: int | None = None,
) -> ScopeBundle:
    """Snapshot the current bundle for a child execution.

    CausalOrigin is always copied (via ``CausalScope.extend``). Authority is
    either the same object (expiration will propagate) or an expired view.
    """
    return require_bundle().child(
        task_id,
        inherit_authority=inherit_authority,
        kind=kind,
        new_pid=new_pid,
        new_tid=new_tid,
    )
