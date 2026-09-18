"""Host teaching prototype of Agent ExecutionScope (not a kernel module)."""

from .context import ScopeBundle, get_bundle, require_bundle, using_bundle
from .kernel_sim import DenyReason, SimulatedKernel, SyscallDenied, SyscallEvent
from .provenance import ProvenanceGraph
from .runtime import AgentRuntime
from .scope import (
    AGENT_BASE_POLICY,
    CODE_TOOL_POLICY,
    SEARCH_TOOL_POLICY,
    AuthorityScope,
    CausalOrigin,
    CausalScope,
    ExecutionScopeID,
    ToolPolicy,
)

__all__ = [
    "AGENT_BASE_POLICY",
    "AuthorityScope",
    "AgentRuntime",
    "CODE_TOOL_POLICY",
    "CausalOrigin",
    "CausalScope",
    "DenyReason",
    "ExecutionScopeID",
    "ProvenanceGraph",
    "SEARCH_TOOL_POLICY",
    "ScopeBundle",
    "SimulatedKernel",
    "SyscallDenied",
    "SyscallEvent",
    "ToolPolicy",
    "get_bundle",
    "require_bundle",
    "using_bundle",
]
