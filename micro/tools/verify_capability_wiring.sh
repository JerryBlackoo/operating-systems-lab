#!/usr/bin/env bash
# 防回归：默认 capability 镜像不得链 *_m6 实验包装。
set -euo pipefail

MICRO="$(cd "$(dirname "$0")/.." && pwd)"
MK="$MICRO/Makefile"
INIT_MK="$MICRO/userland/init/user/Makefile"
fail() {
    echo "[wiring] FAIL $*"
    exit 1
}

rg -q 'kernel_init_m6\.c|service_boot_m6\.c' "$MK" && \
    fail "Makefile must not list kernel_init_m6.c or service_boot_m6.c in C_SRCS"
rg -q 'kernel/boot/kernel_init\.c' "$MK" || fail "Makefile missing kernel_init.c"
rg -q 'kernel/boot/service_boot\.c' "$MK" || fail "Makefile missing service_boot.c"
if rg '^OBJS.*init_main_m6' "$INIT_MK" >/dev/null 2>&1; then
    fail "init Makefile OBJS must not use init_main_m6.o"
fi
rg '^OBJS[[:space:]:=].*init_main\.o' "$INIT_MK" >/dev/null || \
    fail "init Makefile OBJS must list init_main.o"

echo "[wiring] PASS capability default wiring"