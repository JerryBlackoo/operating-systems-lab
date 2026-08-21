#!/bin/bash
# 实验七 - 一键自动部署脚本
# 用法: cd src/实验七 && ./deploy.sh [starter|solution]
# 功能: 环境检查 → 编译 → 格式化磁盘 → 功能验证（电力巡检日志） → fsck/持久化 → 清理

set +e

VERSION=${1:-solution}
DISK_IMG="disk.img"
TARGET_DIR="${VERSION}"

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
tick() { PASS=$((PASS+1)); log_ok "$1"; }
fail() { FAIL=$((FAIL+1)); log_fail "$1"; }

echo ""
echo "=============================================="
echo "  实验七 · 用户态文件系统模拟 - 自动部署 (${VERSION})"
echo "  场景: 电力巡检日志本地持久化存储"
echo "=============================================="
echo ""

# ========== 步骤1: 环境检查 ==========
log_info "步骤1/6: 环境检查..."

HAVE_ALL=1
for t in gcc make; do
    if ! command -v $t &>/dev/null; then
        fail "$t 未安装"
        HAVE_ALL=0
    fi
done
[ "$HAVE_ALL" = "1" ] && tick "基础工具就绪 (gcc/make)"

OS_TYPE=$(uname -s)
log_info "  操作系统: ${OS_TYPE}（用户态模拟，无需内核支持）"
case "$OS_TYPE" in
    Linux|Darwin|MSYS*|MINGW*|CYGWIN*) tick "当前平台兼容" ;;
    *) log_warn "未知平台，编译时可能需要手动调整" ;;
esac

# ========== 步骤2: 编译 ==========
log_info "步骤2/6: 编译文件系统模拟器..."
cd "$(dirname "$0")/${TARGET_DIR}"

make clean >/dev/null 2>&1
if make 2>&1 | grep -qiE "error:|undefined reference"; then
    fail "编译失败"
    make 2>&1 | tail -15
    exit 1
fi

# 找二进制
BIN_NAME=""
for f in fs_test myfs filesys fs; do
    [ -x "$f" ] && { BIN_NAME="$f"; break; }
done
[ -z "$BIN_NAME" ] && BIN_NAME=$(find . -maxdepth 1 -type f -perm /111 ! -name 'Makefile' | head -1 | sed 's|^\./||')

if [ -z "$BIN_NAME" ] || [ ! -x "$BIN_NAME" ]; then
    fail "未找到可执行文件（编译无错误但产物缺失？）"
    exit 1
fi
tick "编译通过: ${BIN_NAME}"

# ========== 步骤3: 格式化虚拟磁盘 ==========
log_info "步骤3/6: 格式化 + 挂载验证..."

rm -f ${DISK_IMG}
OUT=$(./${BIN_NAME} ${DISK_IMG} format 2>&1)
if [ "$?" -eq 0 ] && echo "$OUT" | grep -qi "format successful\|格式化"; then
    tick "format 成功"
else
    fail "format 失败"
    echo "$OUT"; exit 1
fi

if [ -f "${DISK_IMG}" ]; then
    SIZE=$(du -h ${DISK_IMG} | awk '{print $1}')
    tick "磁盘镜像文件 ${DISK_IMG} 已创建 (${SIZE})"
fi

# ========== 步骤4: 基础文件 CRUD 验证 ==========
log_info "步骤4/6: 电力巡检日志 CRUD 验证..."

CSV=/tmp/power_data.csv
cat > "$CSV" <<'EOF'
2026-08-16 10:00:00,station_1,220.5V,10.2A,2249.1W,OK
2026-08-16 10:01:00,station_2,380.0V,15.0A,5700.0W,OVERLOAD
2026-08-16 10:02:00,station_3,219.8V,10.1A,2220.0W,OK
EOF

./${BIN_NAME} ${DISK_IMG} create power_log.csv >/dev/null 2>&1
if [ "$?" -eq 0 ]; then tick "create power_log.csv 成功"; else fail "create power_log.csv 失败"; fi

./${BIN_NAME} ${DISK_IMG} write power_log.csv < "$CSV" >/dev/null 2>&1
if [ "$?" -eq 0 ]; then tick "写入3条巡检记录 (stdin) 成功"; else fail "写入巡检记录失败"; fi

READ_OUT=$(./${BIN_NAME} ${DISK_IMG} read power_log.csv 2>/dev/null)
FOUND=1
for key in "station_1" "220.5V" "OVERLOAD" "station_3"; do
    echo "$READ_OUT" | grep -q "$key" || FOUND=0
done
[ "$FOUND" = "1" ] && tick "读取回校验通过（关键字全部匹配）" || fail "读取数据不完整"

# ls 验证
LS_OUT=$(./${BIN_NAME} ${DISK_IMG} ls 2>/dev/null)
echo "$LS_OUT" | grep -q "power_log.csv" \
    && tick "目录列出: power_log.csv 存在" \
    || fail "目录列出未找到 power_log.csv"

# ========== 步骤5: 电力场景 + 持久化验证 ==========
log_info "步骤5/6: 多传感器文件 + 进程退出持久化验证..."

for name in voltage.csv current.csv temperature.csv fault.log; do
    ./${BIN_NAME} ${DISK_IMG} create "$name" >/dev/null 2>&1
done

COUNT=$(./${BIN_NAME} ${DISK_IMG} ls 2>/dev/null | grep -cE "\.csv|\.log")
if [ "$COUNT" -ge 4 ]; then
    tick "多文件共存: ${COUNT} 个文件 (voltage/current/temperature/fault + power_log)"
else
    fail "文件数量不足: 期望>=4, 实际${COUNT}"
fi

# 模拟断电: 重新开一个进程 ls，验证 disk.img 里的信息真的持久化了
REOPEN=$(./${BIN_NAME} ${DISK_IMG} ls 2>/dev/null)
REMAIN=1
for key in power_log.csv voltage.csv current.csv; do
    echo "$REOPEN" | grep -q "$key" || REMAIN=0
done
[ "$REMAIN" = "1" ] \
    && tick "持久化验证通过 (重新打开镜像后文件完整保留)" \
    || fail "持久化失败 — 重新打开后文件丢失"

# 删除一个文件再验证
./${BIN_NAME} ${DISK_IMG} delete fault.log >/dev/null 2>&1
REOPEN2=$(./${BIN_NAME} ${DISK_IMG} ls 2>/dev/null)
if echo "$REOPEN2" | grep -q "fault.log"; then
    fail "delete fault.log 后文件仍存在"
else
    tick "delete fault.log 成功，文件系统空间回收"
fi

# ========== 步骤6: 清理 + 总结 ==========
log_info "步骤6/6: 清理..."
rm -f "$CSV"
log_info "  保留磁盘镜像: $(pwd)/${DISK_IMG}（可继续 ./${BIN_NAME} ${DISK_IMG} 交互操作）"

echo ""
echo "=============================================="
echo "  实验七自动部署完成"
echo "  PASS: ${PASS}    FAIL: ${FAIL}"
echo "=============================================="
echo ""
echo "电力场景说明:"
echo "  - 模拟变电站采集终端 64MB 本地缓存盘"
echo "  - 经典 Unix 布局: 超级块 → inode位图 → 块位图 → inode表 → 数据区"
echo "  - 支持文件 CRUD、多文件共存、进程退出持久化"
echo "  - 适用场景: 电力巡检日志、告警事件、就地数据缓存"
echo "  - 交互命令: format / create / open / close / read / write / delete / ls / exit"
echo "    小贴士: write/read 除了传 fd 外，也可以直接传文件名（自动 open/close）"
echo "    小贴士: write 没写 payload 的时候自动从 stdin 读，可配合管道/重定向"
echo ""

[ "$FAIL" -eq 0 ] && exit 0 || exit 1
