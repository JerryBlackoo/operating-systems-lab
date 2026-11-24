#!/usr/bin/env python3
"""Verify link metadata and observable behavior, including a missing library."""

import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def execute(*command: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, text=True, capture_output=True, env=env, check=False)


def output(*command: str) -> str:
    env = os.environ.copy()
    env["LC_ALL"] = "C"
    result = execute(*command, env=env)
    if result.returncode:
        raise RuntimeError(f"{' '.join(command)} failed: {result.stderr.strip()}")
    return result.stdout


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def main() -> int:
    build = Path(sys.argv[1] if len(sys.argv) > 1 else "build").resolve()
    env = os.environ.copy()
    for key in ("LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT", "LD_BIND_NOW"):
        env.pop(key, None)

    static = str(build / "static")
    startup = str(build / "startup")
    runtime = str(build / "runtime")
    static_program_headers = output("readelf", "-lW", static)
    startup_dynamic = output("readelf", "-dW", startup)
    runtime_dynamic = output("readelf", "-dW", runtime)
    startup_relocations = output("readelf", "-rW", startup)
    static_symbols = output("nm", static)

    require("INTERP" not in static_program_headers, "static unexpectedly has a dynamic interpreter")
    require(re.search(r"\b[Tt] add$", static_symbols, re.MULTILINE) is not None,
            "static does not define add")
    require("Shared library: [libcalc.so]" in startup_dynamic, "startup does not require libcalc.so")
    require("BIND_NOW" in startup_dynamic, "startup is not configured for immediate binding")
    require("JUMP_SLOT" in startup_relocations and re.search(r"\badd\b", startup_relocations),
            "startup has no dynamic add relocation")
    require("Shared library: [libcalc.so]" not in runtime_dynamic,
            "runtime should not require libcalc.so at startup")
    print("PASS: ELF metadata distinguishes static, startup and runtime linking")

    for name in ("static", "startup", "runtime"):
        result = execute(str(build / name), env=env)
        require(result.returncode == 0, f"{name} failed: {result.stderr.strip()}")
        require("[main] add(2,3) = 5" in result.stdout, f"{name} produced an unexpected result")
        if name == "runtime":
            require("[main] before dlopen" in result.stdout, "runtime did not reach dlopen")
    print("PASS: all three programs calculate add(2,3) = 5")

    # Fresh directory: do not remove or rename the real shared library.
    with tempfile.TemporaryDirectory(prefix="elf-link-missing-") as temporary:
        missing = Path(temporary)
        for name in ("static", "startup", "runtime"):
            shutil.copy2(build / name, missing / name)

        static_result = execute(str(missing / "static"), env=env)
        startup_result = execute(str(missing / "startup"), env=env)
        runtime_result = execute(str(missing / "runtime"), env=env)
        require(static_result.returncode == 0 and "add(2,3) = 5" in static_result.stdout,
                "static failed without libcalc.so")
        require(startup_result.returncode != 0 and "[main] started" not in startup_result.stdout,
                "startup should fail before main without libcalc.so")
        require("libcalc.so" in startup_result.stderr, "startup failure did not identify libcalc.so")
        require(runtime_result.returncode != 0 and "[main] before dlopen" in runtime_result.stdout,
                "runtime should reach dlopen before failing")
        require("dlopen failed" in runtime_result.stderr and "libcalc.so" in runtime_result.stderr,
                "runtime failure did not identify the missing library")
    print("PASS: without libcalc.so: static succeeds; startup fails before main; runtime fails at dlopen")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        sys.exit(1)
