#!/usr/bin/env bash
# RustMicroOS: 全链路网络体检。直接运行，不改任何配置。
set -u
PROXY="${PROXY_URL:-http://127.0.0.1:7890}"
echo "=== proxy env ==="; env | grep -iE '^(http|https|all|no)_proxy=' || echo '(none)'
echo; echo "=== Windows system proxy ==="
reg query 'HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings' //v ProxyEnable 2>/dev/null | tr -d '\r'
reg query 'HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings' //v ProxyServer 2>/dev/null | tr -d '\r'
echo; echo "=== port 7890 ==="
netstat -ano | grep LISTENING | grep ':7890' | tr -d '\r' || echo '7890 NOT LISTENING'
echo; echo "=== endpoint matrix ==="
for u in \
  https://github.com \
  https://gitlab.eduxiji.net \
  https://index.crates.io \
  https://static.rust-lang.org \
  https://www.bing.com \
  https://www.qemu.org \
  https://raw.githubusercontent.com \
  https://html.duckduckgo.com \
; do
  direct=$(curl -4 -sS -o /dev/null -L --max-time 8 -w '%{http_code}' "$u" 2>&1)
  proxied=$(curl -4 -sS -o /dev/null -L -x "$PROXY" --max-time 8 -w '%{http_code}' "$u" 2>&1)
  printf '%-34s direct=%-4s via7890=%s\n' "$u" "$direct" "$proxied"
done
