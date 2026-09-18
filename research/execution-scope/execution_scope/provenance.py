"""Provenance graph: reconstruct Agent → tool_call → Tool → Task → syscall."""

from __future__ import annotations

import threading
from dataclasses import dataclass, field
from typing import Iterable

from .context import ScopeBundle
from .kernel_sim import SyscallEvent


@dataclass
class Node:
    node_id: str
    kind: str
    label: str
    extra: dict = field(default_factory=dict)


@dataclass(frozen=True)
class Edge:
    src: str
    dst: str
    kind: str  # spawned | caused


def bundle_node_id(bundle: ScopeBundle) -> str:
    return f"scope:{bundle.scope_id.uid}"


def syscall_node_id(event: SyscallEvent) -> str:
    return f"sys:{event.event_id}"


class ProvenanceGraph:
    def __init__(self) -> None:
        self.nodes: dict[str, Node] = {}
        self.edges: list[Edge] = []
        self._syscall_index: dict[str, str] = {}
        self._lock = threading.Lock()

    def add_node(self, node: Node) -> Node:
        with self._lock:
            existing = self.nodes.get(node.node_id)
            if existing is None:
                self.nodes[node.node_id] = node
                return node
            existing.extra.update(node.extra)
            return existing

    def add_edge(self, src: str, dst: str, kind: str) -> None:
        edge = Edge(src=src, dst=dst, kind=kind)
        with self._lock:
            if edge not in self.edges:
                self.edges.append(edge)

    def add_bundle(self, bundle: ScopeBundle, kind: str | None = None) -> str:
        node_id = bundle_node_id(bundle)
        last = bundle.causal.frames[-1] if bundle.causal.frames else None
        self.add_node(
            Node(
                node_id=node_id,
                kind=kind or (last.kind if last else "scope"),
                label=str(bundle.scope_id),
                extra={
                    "agent_id": bundle.scope_id.agent_id,
                    "tool_call_id": bundle.scope_id.tool_call_id,
                    "tool_name": bundle.scope_id.tool_name,
                    "task_id": bundle.scope_id.task_id,
                    "pid": bundle.pid,
                    "tid": bundle.tid,
                    "origin": str(bundle.causal.origin),
                },
            )
        )
        return node_id

    def add_spawn(self, parent: ScopeBundle, child: ScopeBundle, *, kind: str = "task") -> None:
        src = self.add_bundle(parent)
        dst = self.add_bundle(child, kind=kind)
        self.add_edge(src, dst, "spawned")

    def record_syscall(self, event: SyscallEvent, parent: ScopeBundle | None) -> str:
        sid = syscall_node_id(event)
        origin = str(event.causal_origin) if event.causal_origin else "<none>"
        self.add_node(
            Node(
                node_id=sid,
                kind="syscall",
                label=event.summary(),
                extra={
                    "event_id": event.event_id,
                    "op": event.op.value,
                    "resource": event.resource,
                    "allowed": event.allowed,
                    "deny_reason": event.deny_reason.value if event.deny_reason else None,
                    "pid": event.pid,
                    "tid": event.tid,
                    "origin": origin,
                },
            )
        )
        if parent is not None:
            src = self.add_bundle(parent)
            self.add_edge(src, sid, "caused")
        with self._lock:
            self._syscall_index[event.event_id] = sid
        return sid

    def predecessors(self, node_id: str) -> list[str]:
        return [edge.src for edge in self.edges if edge.dst == node_id]

    def reconstruct(self, event: SyscallEvent | str) -> list[Node]:
        """Walk caused/spawned edges from a syscall back to the Agent root."""
        event_id = event if isinstance(event, str) else event.event_id
        current = self._syscall_index.get(event_id, f"sys:{event_id}")
        seen: set[str] = set()
        rev: list[Node] = []
        while current and current not in seen:
            seen.add(current)
            node = self.nodes.get(current)
            if node is None:
                break
            rev.append(node)
            preds = self.predecessors(current)
            current = preds[0] if preds else None
        rev.reverse()
        return rev

    def chain_labels(self, event: SyscallEvent | str) -> list[str]:
        return [node.label for node in self.reconstruct(event)]

    def format_chain(self, event: SyscallEvent | str) -> str:
        nodes = self.reconstruct(event)
        if not nodes:
            return "<empty>"
        return " → ".join(f"{node.kind}:{node.label}" if node.kind != "syscall" else node.label for node in nodes)

    def all_syscall_nodes(self) -> Iterable[Node]:
        return [n for n in self.nodes.values() if n.kind == "syscall"]
