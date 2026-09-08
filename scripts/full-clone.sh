#!/usr/bin/env bash
# Convert shallow clones under external/os-learning and external/cpu-learning
# to full-history repositories (git fetch --unshallow).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

unshallow_if_needed() {
  local dir="$1"
  local extra_fetch="${2:-}"
  local name
  name="$(basename "$dir")"

  if [ ! -d "$dir/.git" ]; then
    return 0
  fi

  if [ "$(git -C "$dir" rev-parse --is-shallow-repository)" != "true" ]; then
    echo "skip  $name (already full)"
    return 0
  fi

  echo "unshallow $name ..."
  # shellcheck disable=SC2086
  git -C "$dir" fetch --unshallow $extra_fetch
  echo "done    $name"
}

process_dir() {
  local base="$1"
  local extra_fetch="${2:-}"

  for entry in "$base"/*; do
    [ -d "$entry" ] || continue
    [ -L "$entry" ] && continue
    [ -d "$entry/.git" ] || continue
    unshallow_if_needed "$entry" "$extra_fetch"
  done
}

echo "=== OS learning repos ==="
for entry in "$ROOT/external/os-learning"/*; do
  [ -d "$entry" ] || continue
  [ -L "$entry" ] && continue
  [ -d "$entry/.git" ] || continue
  if [ "$(basename "$entry")" = "linux" ]; then
    unshallow_if_needed "$entry" "--tags"
  else
    unshallow_if_needed "$entry"
  fi
done

echo ""
echo "=== CPU learning repos ==="
process_dir "$ROOT/external/cpu-learning"

echo ""
echo "Done. All shallow repositories have been converted to full clones."
