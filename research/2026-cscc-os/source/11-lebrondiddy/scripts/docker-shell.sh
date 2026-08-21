#!/usr/bin/env bash
set -euo pipefail

IMAGE="${IMAGE:-zhouzhouyi/os-contest:20260510}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

exec docker run --rm -it \
  -v "${ROOT_DIR}:/workspace" \
  -w /workspace \
  "${IMAGE}" \
  bash

