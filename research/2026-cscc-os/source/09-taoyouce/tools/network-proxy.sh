#!/usr/bin/env bash
# RustMicroOS: 网络代理开关（自动检测本机 Clash/代理 127.0.0.1:7890）
#
# 用法（必须 source，export 才会留在当前 shell）:
#   source tools/network-proxy.sh          # 检测到 7890 则启用，并打印诊断
#   source tools/network-proxy.sh on
#   source tools/network-proxy.sh off
#   source tools/network-proxy.sh status
#
# 直接执行 `bash tools/network-proxy.sh` 只做诊断，不会改你当前终端的 env。
set -u
PROXY_HOST=127.0.0.1
PROXY_PORT="${PROXY_PORT:-7890}"
PROXY_URL="http://$PROXY_HOST:$PROXY_PORT"

proxy_listening() {
  timeout 2 bash -c "exec 3<>/dev/tcp/$PROXY_HOST/$PROXY_PORT" 2>/dev/null
}

set_proxy_env() {
  export HTTP_PROXY="$PROXY_URL"
  export HTTPS_PROXY="$PROXY_URL"
  export ALL_PROXY="$PROXY_URL"
  export http_proxy="$PROXY_URL"
  export https_proxy="$PROXY_URL"
  export all_proxy="$PROXY_URL"
  export NO_PROXY="localhost,127.0.0.1,::1"
  export no_proxy="localhost,127.0.0.1,::1"
  # 仅给 GitHub 远程加代理；GitLab/国内源保持直连
  git config --local http.https://github.com/.proxy "$PROXY_URL" 2>/dev/null || true
}

unset_proxy_env() {
  unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy
  export NO_PROXY="localhost,127.0.0.1,::1"
  export no_proxy="localhost,127.0.0.1,::1"
  git config --local --unset-all http.https://github.com/.proxy 2>/dev/null || true
}

status_report() {
  if proxy_listening; then
    echo "[network-proxy] proxy OK: $PROXY_URL"
  else
    echo "[network-proxy] WARNING: $PROXY_URL is NOT listening (is Clash running?)"
  fi
  printf '[network-proxy] env: HTTP_PROXY=%s HTTPS_PROXY=%s\n' "${HTTP_PROXY-}" "${HTTPS_PROXY-}"
  printf '[network-proxy] git github proxy: %s\n' "$(git config --local --get http.https://github.com/.proxy 2>/dev/null || echo none)"
  echo '[network-proxy] quick checks:'
  curl -sS -o /dev/null -x "$PROXY_URL" --max-time 10 -w '  github via proxy    -> %{http_code} (%{time_total}s)\n' https://github.com 2>&1 || true
  curl -sS -o /dev/null --max-time 10 -w '  gitlab direct       -> %{http_code} (%{time_total}s)\n' https://gitlab.eduxiji.net 2>&1 || true
  curl -sS -o /dev/null --max-time 10 -w '  crates.io direct    -> %{http_code} (%{time_total}s)\n' https://index.crates.io 2>&1 || true
}

CMD="${1:-}"
if [ "$CMD" = off ]; then
  unset_proxy_env
  echo '[network-proxy] disabled'
  exit 0
fi

if [ "$CMD" = status ]; then
  status_report
  exit 0
fi

if proxy_listening; then
  set_proxy_env
  echo "[network-proxy] enabled for this shell"
else
  echo "[network-proxy] proxy not listening; NOT exporting proxy vars"
fi
status_report
