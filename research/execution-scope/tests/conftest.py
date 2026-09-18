from __future__ import annotations

import pytest

from execution_scope.runtime import AgentRuntime


@pytest.fixture
def rt() -> AgentRuntime:
    runtime = AgentRuntime()
    try:
        yield runtime
    finally:
        runtime.drain(timeout=1.0)
        runtime.close()
