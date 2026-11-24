#!/usr/bin/env bash
set -euo pipefail

if ! command -v strace >/dev/null; then
    printf '%s\n' '需要 strace 才能记录系统调用；程序构建和 make check 不依赖它。' >&2
    exit 1
fi

build=$(cd "${1:-build}" && pwd)
mkdir -p "$build/traces"

for program in static startup runtime; do
    trace="$build/traces/$program.trace"
    strace -qq -e trace=execve,openat,mmap,mprotect,write -o "$trace" \
        "$build/$program" >"$build/traces/$program.stdout"
    printf '\n%s\n' "[$program] 关键事件："
    awk '/^execve\(/ || /^write\(1, "\[main\]/ || /openat\(.*libcalc\.so".* = [0-9]+$/' "$trace"
    printf '完整记录：%s\n' "$trace"
done
