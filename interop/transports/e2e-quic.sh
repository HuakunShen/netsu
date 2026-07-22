#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
compose_file="$repo_root/interop/transports/docker-compose.quic.yml"
results_dir="$repo_root/interop/transports/results"
cd "$repo_root"

cleanup() {
  docker compose -f "$compose_file" down -v --remove-orphans
}
trap cleanup EXIT

echo "==> building QUIC E2E image"
docker compose -f "$compose_file" build
echo "==> starting isolated QUIC peers"
docker compose -f "$compose_file" up -d
echo "==> running QUIC/netem correctness matrix"
if ! bun interop/transports/run-quic-matrix.ts; then
  mkdir -p "$results_dir"
  docker compose -f "$compose_file" logs --no-color >"$results_dir/compose.log" 2>&1 || true
  exit 1
fi
