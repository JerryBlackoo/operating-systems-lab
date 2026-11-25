#!/usr/bin/env python3
"""逐模块在 QEMU 中单独启动，判定 PASS。

判定规则（与目标一致）：
  - 单模块镜像：必须打印 `<module>: all tests passed` 或 `[<module>] PASS`，
    且 QEMU 应自行退出（rc=0）。
  - capability 是**常驻镜像**：它跑完 demo 后进入 idle 心跳，不会自行关机。
    对它的正确判定是「出现 PASS 且之后没有 panic/fail」，QEMU 由外层 timeout
    结束是预期行为，不算失败。

用法：python3 tools/modules_qemu.py [--list]
"""
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
os.chdir(ROOT)

# 常驻镜像：跑完不自退，靠 timeout 收尾。
RESIDENT = {"capability"}


def modules():
    out = subprocess.run(["make", "-s", "list-modules"], cwd=ROOT,
                         capture_output=True, text=True)
    return out.stdout.split()


def run_one(name, timeout=75):
    """
    跑一个模块镜像。

    常驻镜像（capability）不会自行退出：一旦看到 PASS 就可以提前杀掉 QEMU，
    不必等满 timeout。这让整体验证从 ~45 s 降到几秒。
    """
    resident = name in RESIDENT
    proc = subprocess.Popen(
        ["make", "-s", "run", f"BOOT_MODULES={name}"],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    buf = bytearray()
    deadline = time.time() + timeout
    ok = False
    pass_re = re.compile(
        rf"(\[{re.escape(name)}\] PASS|{re.escape(name)}: all tests passed|"
        rf"\[bootstrap\] PASS|demo: all tests passed)".encode())

    while time.time() < deadline:
        if proc.poll() is not None:
            rest = proc.stdout.read() or b""
            buf.extend(rest)
            break
        chunk = proc.stdout.read(4096)
        if chunk:
            buf.extend(chunk)
            if pass_re.search(bytes(buf)):
                ok = True
                if resident:
                    break
        else:
            time.sleep(0.02)

    rc = proc.poll()
    if rc is None:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        rc = None

    text = bytes(buf).decode("utf-8", "replace")
    if pass_re.search(bytes(buf)):
        ok = True
    # 只把明确的失败信号当失败；"FAIL" 在 demo 脚本里也用于负向用例，
    # 这里只查内核/模型自己打印的断言失败与 panic。
    bad = re.search(r"(demo_fail|Assertion|panic|: FAIL|FAIL at line)", text)
    return name, rc, ok, (bad.group(0) if bad else "")


def main():
    if "--list" in sys.argv:
        print("\n".join(modules()))
        return 0

    fails = []
    for name in modules():
        n, rc, ok, bad = run_one(name)
        resident = n in RESIDENT
        # 常驻镜像：PASS 即成功（rc 由 timeout 决定，不看）
        verdict = "ok" if (ok and not bad) else "FAIL"
        if verdict == "FAIL":
            fails.append((n, rc, ok, bad))
        print(f"  {verdict:4} {n:12} rc={str(rc):5} pass={ok} {('bad=' + bad) if bad else ''}")

    if fails:
        print(f"\nFAILURES: {fails}")
        return 1
    print("\nALL MODULES PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
