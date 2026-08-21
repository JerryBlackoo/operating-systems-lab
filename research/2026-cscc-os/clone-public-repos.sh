#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-2026-cscc-os-repos}"
mkdir -p "$ROOT"
cd "$ROOT"

clone_if_missing() {
  local name="$1"
  local url="$2"
  if [[ -d "$name/.git" ]]; then
    echo "[skip] $name already exists"
    return
  fi
  echo "[clone] $name"
  git clone "$url" "$name" || {
    echo "[warn] failed to clone $url"
    echo "       repository may require login, have moved, or be temporarily unavailable"
  }
}

clone_if_missing \
  "suep-proj54-openEuler-step-by-step" \
  "https://gitlab.eduxiji.net/T2026102569910321/project3136859-386827.git"

clone_if_missing \
  "hust-proj59-llm-memory" \
  "https://gitlab.eduxiji.net/T2026104879911351/project3136859-389097.git"

clone_if_missing \
  "hfut-2026os" \
  "https://gitlab.eduxiji.net/T202510359998209/2026os.git"

clone_if_missing \
  "heu-you-only-live-once-archive" \
  "https://gitlab.eduxiji.net/educg-group-43734-3136859/T2026102179910632-23.git"

clone_if_missing \
  "whu-oskernel-xiaonanliang-archive" \
  "https://gitlab.eduxiji.net/educg-group-43501-3132633/T2026104869910817-342.git"

clone_if_missing \
  "sdu-lebrondiddy-archive" \
  "https://gitlab.eduxiji.net/educg-group-43501-3132633/T2026104229910571-2751.git"

echo
echo "Done."
echo "Some competition GitLab repositories may require authentication."
echo "For Eureka, 0xDEADBEEF, sudo_win_the_cscc, TraceForce/AgentMem and proj51,"
echo "see REPOSITORIES.md and search by project/team ID until stable clone URLs are verified."
