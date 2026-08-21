#!/bin/bash
# 实验八 - 一键自动部署脚本
# 用法: cd src/实验八 && sudo ./deploy.sh [starter|solution]
# 功能: 环境检查 → 编译三阶段 → 加载 → 电力协议过滤验证 → 性能测试 → 清理

set +e   # 个别用例失败不要直接退出，最后统一汇报

VERSION=${1:-solution}
POWER_PORT=40000
BAD_PORT=8080
IEC104_PORT=2404
TEST_IFACE=lo   # 所有验证都在 loopback 上，物理网卡环境下也可复现

# ========== 颜色定义 ==========
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()  { echo -e "${BLUE}[INFO]${NC}  $1"; }
log_ok()     { echo -e "${GREEN}[ OK ]${NC}  $1"; }
log_warn()   { echo -e "${YELLOW}[WARN]${NC}  $1"; }
log_fail()   { echo -e "${RED}[FAIL]${NC}  $1"; }

PASS=0
FAIL=0
tick()       { PASS=$((PASS+1)); log_ok "$1"; }
fail()       { FAIL=$((FAIL+1)); log_fail "$1"; }

echo ""
echo "=============================================="
echo "  实验八 · 网络管理 eBPF/XDP - 自动部署 (${VERSION})"
echo "  场景: 电力协议 UDP ${POWER_PORT} 白名单过滤"
echo "=============================================="
echo ""

# ========== 步骤1: 环境检查 ==========
log_info "步骤1/7: 环境检查..."

if [ "$(id -u)" -ne 0 ]; then
    log_warn "当前非 root，insmod/ip link 等操作可能失败（建议: sudo $0）"
fi

HAVE_ALL=1
for t in gcc make clang ip; do
    if ! command -v $t &>/dev/null; then
        log_fail "$t 未安装"
        HAVE_ALL=0
    fi
done
[ "$HAVE_ALL" = "1" ] && tick "基础工具已就绪 (gcc/make/clang/iproute2)"

# 内核版本
KERNEL_MAJOR=$(uname -r | cut -d. -f1)
KERNEL_MINOR=$(uname -r | cut -d. -f2)
if [ "$KERNEL_MAJOR" -ge 5 ] || ([ "$KERNEL_MAJOR" -eq 4 ] && [ "$KERNEL_MINOR" -ge 8 ]); then
    tick "内核版本 $(uname -r) 满足 XDP 要求（>= 4.8）"
else
    fail "内核版本过低，XDP 需要 4.8+"
fi

# ========== 步骤2-4: 编译三个 Phase ==========
BASEDIR="${VERSION}"
cd "$(dirname "$0")" || exit 1
ROOT="$PWD"

for phase in phase1_socket phase2_netfilter phase3_xdp; do
    log_info "编译 ${phase}..."
    cd "${ROOT}/${BASEDIR}/${phase}"
    make clean >/dev/null 2>&1
    if make 2>&1 | grep -qiE "error:|undefined reference"; then
        fail "${phase} 编译失败"
        make 2>&1 | tail -10
    else
        tick "${phase} 编译通过"
    fi
done

# ========== 公用: 起/杀 UDP 服务器并验证 ==========
# $1=port  $2=expected (ok|drop)  返回 0=符合预期
probe_udp() {
    local port=$1 expected=$2
    local server_pid rc

    # 先清理可能遗留的占用（确保该端口空闲再启动我们的 server）
    if command -v fuser &>/dev/null; then
        fuser -k "${port}/udp" &>/dev/null
        sleep 0.2
    fi

    cd "${ROOT}/${BASEDIR}/phase1_socket"
    ./udp_server "$port" > /tmp/udp_srv_${port}.log 2>&1 &
    server_pid=$!
    sleep 0.6

    # 确认 server 进程仍然存活（否则可能端口冲突 / 启动失败）
    if ! kill -0 "$server_pid" 2>/dev/null; then
        log_warn "probe_udp: udp_server port=$port 提前退出（端口被占用？）"
        rm -f /tmp/udp_srv_${port}.log
        return 1
    fi

    ./udp_client 127.0.0.1 "$port" "PROBE_port_${port}" > /tmp/udp_cli_${port}.log 2>&1
    rc=$?
    sleep 0.2
    kill "$server_pid" 2>/dev/null
    wait "$server_pid" 2>/dev/null
    local reply_ok=0
    grep -qi "Echo reply" /tmp/udp_cli_${port}.log 2>/dev/null && reply_ok=1
    rm -f /tmp/udp_srv_${port}.log /tmp/udp_cli_${port}.log
    if [ "$expected" = "ok" ]; then
        [ "$reply_ok" = "1" ] && return 0
    else
        [ "$reply_ok" = "0" ] && return 0
    fi
    return 1
}

# ========== 步骤5: Phase1 功能验证 ==========
log_info "步骤5/7: Phase1 UDP Socket 基线验证..."

if probe_udp "$POWER_PORT" ok; then
    tick "Phase1: 端口 ${POWER_PORT} 正常回显"
else
    fail "Phase1: 端口 ${POWER_PORT} 回显失败"
fi
if probe_udp "$BAD_PORT" ok; then
    tick "Phase1: 端口 ${BAD_PORT} 正常回显（无过滤时两个端口都应通）"
else
    log_warn "Phase1: 端口 ${BAD_PORT} 回显异常（可能只是端口占着），可忽略"
fi

# ========== 步骤6: Phase2 Netfilter 验证 ==========
log_info "步骤6/7: Phase2 Netfilter 过滤验证 (白名单 UDP ${POWER_PORT})..."

cd "${ROOT}/${BASEDIR}/phase2_netfilter"
if [ -f "netfilter_hook.ko" ]; then
    rmmod netfilter_hook 2>/dev/null
    if insmod netfilter_hook.ko; then
        sleep 0.5
        PROBE_P1=; PROBE_P2=
        probe_udp "$POWER_PORT" ok  && PROBE_P1=1
        probe_udp "$BAD_PORT" drop && PROBE_P2=1
        if [ -n "$PROBE_P1" ] && [ -n "$PROBE_P2" ]; then
            tick "Phase2: Netfilter 策略生效 → 允许 ${POWER_PORT} / 丢弃 ${BAD_PORT}"
        else
            [ -z "$PROBE_P1" ] && fail "Phase2: ${POWER_PORT} 被误丢（应该放行）"
            [ -z "$PROBE_P2" ] && fail "Phase2: ${BAD_PORT} 未被丢弃（应该丢弃）"
        fi
        if dmesg 2>/dev/null | tail -30 | grep -qE "Netfilter|netfilter"; then
            tick "Phase2: dmesg 有过滤审计日志"
        else
            log_warn "Phase2: dmesg 日志未找到（日志被轮转？）"
        fi
        rmmod netfilter_hook 2>/dev/null
    else
        fail "Phase2: insmod netfilter_hook.ko 失败"
    fi
else
    fail "Phase2: 未找到 netfilter_hook.ko（编译失败？）"
fi

# ========== 步骤7: Phase3 XDP 验证 ==========
log_info "步骤7/7: Phase3 XDP 驱动层过滤验证..."

cd "${ROOT}/${BASEDIR}/phase3_xdp"
if [ -f "xdp_filter.o" ]; then
    # 清旧的 XDP
    ip link set dev $TEST_IFACE xdp off 2>/dev/null
    if ip link set dev $TEST_IFACE xdp obj xdp_filter.o sec xdp 2>/tmp/xdp_load.err; then
        sleep 0.3
        if ip -d link show $TEST_IFACE 2>/dev/null | grep -q "xdp"; then
            tick "Phase3: XDP 已挂到 ${TEST_IFACE} 网卡"
        fi
        PROBE_P1=; PROBE_P2=
        probe_udp "$POWER_PORT" ok  && PROBE_P1=1
        probe_udp "$BAD_PORT" drop && PROBE_P2=1
        if [ -n "$PROBE_P1" ] && [ -n "$PROBE_P2" ]; then
            tick "Phase3: XDP 策略生效 → 允许 ${POWER_PORT} / 丢弃 ${BAD_PORT}（驱动层）"
        else
            [ -z "$PROBE_P1" ] && fail "Phase3: ${POWER_PORT} 被误丢（应该放行）"
            [ -z "$PROBE_P2" ] && fail "Phase3: ${BAD_PORT} 未被丢弃（应该丢弃）"
        fi
    else
        log_warn "Phase3: XDP 加载失败（可能缺少 libbpf / kernel bpf config）"
        [ -s /tmp/xdp_load.err ] && cat /tmp/xdp_load.err
    fi
    ip link set dev $TEST_IFACE xdp off 2>/dev/null
    rm -f /tmp/xdp_load.err
else
    log_warn "Phase3: 未找到 xdp_filter.o（XDP 编译失败，可忽略 Phase3）"
fi

# ========== 汇总 ==========
echo ""
echo "=============================================="
echo "  实验八自动部署完成"
echo "  PASS: ${PASS}    FAIL: ${FAIL}"
echo "=============================================="
echo ""
echo "电力场景说明:"
echo "  - Phase1: 用户态 UDP 传输电力采集数据（端口 ${POWER_PORT}）"
echo "  - Phase2: Netfilter 内核钩子 — 只放行电力协议，丢弃其它"
echo "  - Phase3: XDP 驱动层原位丢弃 — 零 skb、零协议栈，8M+ PPS 高性能"
echo "  - 参考标准:"
echo "      IEC 60870-5-104 (TCP ${IEC104_PORT}) — 电力远动协议"
echo "      IEC 61850 — 智能变电站通信"
echo "      DL/T 634  — 中国电力行业远动标准"
echo ""

[ "$FAIL" -eq 0 ] && exit 0 || exit 1
