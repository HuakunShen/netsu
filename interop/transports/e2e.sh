#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
compose_file="$repo_root/interop/transports/docker-compose.yml"
export COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-netsu-webrtc-e2e-$$}"
cd "$repo_root"

cleanup() {
  docker compose -f "$compose_file" down -v --remove-orphans
}
trap cleanup EXIT INT TERM

echo "==> building self-contained WebRTC E2E images"
if [[ "${NETSU_E2E_SKIP_BUILD:-0}" != "1" ]]; then
  docker compose -f "$compose_file" build
else
  echo "==> reusing prebuilt E2E images"
fi
echo "==> starting local Wrangler/workerd and direct-only peers"
docker compose -f "$compose_file" up -d --wait
echo "==> running Rust/Chromium WebRTC correctness matrix"
bun interop/transports/run-webrtc-matrix.ts
