#!/usr/bin/env bash
# Operating Systems Lab — 验收测试
# 用法: sudo tests/run_tests.sh --rootfs ./busybox-rootfs
# 退出码: 0 = 全部通过, 1 = 有失败
set -u

ROOTFS=""
CTR=""
PASS=0
FAIL=0
declare -a LEFTOVER=()

usage() { echo "usage: $0 --rootfs DIR [--ctr BIN]"; exit 2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --rootfs) ROOTFS="$2"; shift 2 ;;
    --ctr) CTR="$2"; shift 2 ;;
    *) usage ;;
  esac
done
[ -n "$ROOTFS" ] || usage
[ -n "$CTR" ] || CTR="./ctr"

report() {
  local name="$1" ok="$2"
  if [ "$ok" = "1" ]; then PASS=$((PASS+1)); echo "PASS: $name";
  else FAIL=$((FAIL+1)); echo "FAIL: $name"; fi
}

cleanup() {
  for c in "${LEFTOVER[@]}"; do
    "$CTR" stop "$c" 2>/dev/null
    "$CTR" rm "$c" 2>/dev/null
  done
  umount -l /tmp/ctr-test-upper 2>/dev/null || true
}
trap cleanup EXIT

# t1: 基本运行
out=$("$CTR" run --name c1 "$ROOTFS" /bin/echo hello 2>/dev/null); rc=$?
[ "$rc" = "0" ] && [ "$out" = "hello" ]; report "t1 basic run+echo" $?
LEFTOVER+=(c1)

# t2: PID/UTS namespace 隔离
out=$("$CTR" run --name h1 "$ROOTFS" /bin/sh -c 'hostname; ps 2>/dev/null | wc -l' 2>/dev/null); rc=$?
host=$(echo "$out" | head -1)
np=$(echo "$out" | sed -n 2p)
[ "$rc" = "0" ] && [ "$host" = "h1" ] && [ "${np:-99}" -le 3 ]; report "t2 hostname+pid isolation" $?
LEFTOVER+=(h1)

# t3: mount namespace 隔离（宿主 marker 不可见）
marker=/tmp/ctr-host-marker-$$
echo x > "$marker"
out=$("$CTR" run --name c2 "$ROOTFS" /bin/ls "$marker" 2>&1); rc=$?
rm -f "$marker"
[ "$rc" != "0" ] || [[ "$out" == *"No such file"* ]]; report "t3 host files invisible" $?
LEFTOVER+=(c2)

# t4: cgroup 内存限制强制
out=$("$CTR" run --name mem1 --memory 64m "$ROOTFS" \
  /bin/sh -c 'head -c 200m /dev/zero | tail -c 1' 2>&1); rc=$?
# 期望: 被 OOM 杀死(rc 137) 或分配失败(rc 非0)
[ "$rc" != "0" ]; report "t4 memory limit enforced (rc=$rc)" $?
LEFTOVER+=(mem1)

# t5: cgroup cpu.max
"$CTR" run --name cpu1 --cpus 0.5 "$ROOTFS" /bin/sh -c 'sleep 3' 2>/dev/null &
ctr_pid=$!
sleep 1
cgroup_path=$(cat /proc/$ctr_pid/cgroup | cut -d: -f3)
cpu_max=$(cat "/sys/fs/cgroup${cgroup_path}/cpu.max" 2>/dev/null)
wait $ctr_pid 2>/dev/null
[ "$cpu_max" = "50000 100000" ]; report "t5 cpu.max=$cpu_max" $?
LEFTOVER+=(cpu1)

# t6: overlay 隔离 — 容器写不污染 rootfs
"$CTR" run --name c3 "$ROOTFS" /bin/sh -c 'echo polluted > /should-not-exist' 2>/dev/null
[ ! -f "$ROOTFS/should-not-exist" ]; report "t6 overlay write isolation" $?
LEFTOVER+=(c3)

echo "================================"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ] && exit 0 || exit 1
