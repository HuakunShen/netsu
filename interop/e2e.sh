#!/usr/bin/env bash
# Full local e2e: build the three images (netsu-rs compiles inside its own
# multi-stage image — no host Rust cross-compile needed), run the matrix, tear
# down.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "==> building images"
docker compose -f interop/docker-compose.yml build

echo "==> starting containers"
docker compose -f interop/docker-compose.yml up -d

cleanup() {
  echo "==> tearing down"
  docker compose -f interop/docker-compose.yml down -v --remove-orphans
}
trap cleanup EXIT

echo "==> running matrix"
bun interop/run-matrix.ts
