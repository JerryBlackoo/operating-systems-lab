from execution_scope.scope import (
    CODE_TOOL_POLICY,
    SEARCH_TOOL_POLICY,
    AuthorityScope,
    CausalOrigin,
    ExecutionScopeID,
    SyscallOp,
)


def test_scope_id_chain_labels() -> None:
    sid = ExecutionScopeID(agent_id="A", tool_call_id="tc-1", tool_name="SearchTool", task_id="bg")
    assert sid.chain_labels() == (
        "Agent=A",
        "tool_call=tc-1",
        "Tool=SearchTool",
        "Task=bg",
    )
    assert "SearchTool" in str(sid)


def test_causal_origin_frozen_from_tool_scope() -> None:
    sid = ExecutionScopeID(agent_id="A", tool_call_id="tc-1", tool_name="SearchTool")
    origin = CausalOrigin.from_scope_id(sid)
    later = sid.with_task("bg-9")
    assert origin.tool_name == "SearchTool"
    assert origin.tool_call_id == "tc-1"
    assert later.task_id == "bg-9"
    assert origin.origin_scope_id.task_id is None


def test_search_tool_net_yes_etc_no() -> None:
    p = SEARCH_TOOL_POLICY
    assert p.allows(SyscallOp.CONNECT, "evil.com:443")
    assert not p.allows(SyscallOp.OPEN, "/etc/passwd")
    assert not p.allows(SyscallOp.OPEN, "/workspace/x.py", write=True)


def test_code_tool_workspace_yes_public_net_no() -> None:
    p = CODE_TOOL_POLICY
    assert p.allows(SyscallOp.OPEN, "/workspace/x.py", write=True)
    assert p.allows(SyscallOp.UNLINK, "/workspace/tmp/cache")
    assert not p.allows(SyscallOp.CONNECT, "api.example.com:443")
    assert not p.allows(SyscallOp.OPEN, "/etc/shadow", write=True)
    assert not p.allows(SyscallOp.EXEC, "/bin/sh")


def test_authority_expire_is_shared_via_inherit() -> None:
    sid = ExecutionScopeID(agent_id="A", tool_name="CodeTool")
    auth = AuthorityScope(sid, CODE_TOOL_POLICY)
    child = auth.inherit()
    assert child is auth
    assert auth.is_active()
    auth.expire()
    assert child.expired
    assert not child.is_active()


def test_expired_view_does_not_alias_parent() -> None:
    sid = ExecutionScopeID(agent_id="A", tool_name="SearchTool")
    auth = AuthorityScope(sid, SEARCH_TOOL_POLICY)
    view = auth.expired_view()
    assert view.expired
    assert auth.is_active()
    auth.expire()
    assert view is not auth
