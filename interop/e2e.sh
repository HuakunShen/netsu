#!/usr/bin/env bash
# Full local e2e: cross-compile Rust, build images, run the matrix, tear down.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

arch="$(uname -m | sed 's/arm64/aarch64/;s/amd64/x86_64/')"
export NETSU_RS_BIN="interop/bin/netsu-rs-$arch"

echo "==> cross-compiling netsu-rs"
./interop/build-rust.sh

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
