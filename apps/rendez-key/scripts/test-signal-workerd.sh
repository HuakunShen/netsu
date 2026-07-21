#!/usr/bin/env bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$app_dir/../.." && pwd)"
temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/netsu-signal-workerd.XXXXXX")"
state_dir="$temp_dir/state"
worker_log="$temp_dir/worker.log"
sentinels="$temp_dir/sentinels.txt"
port="${SIGNAL_WORKER_PORT:-18787}"
worker_pid=""

shutdown_worker() {
  if [[ -z "$worker_pid" ]] || ! kill -0 "$worker_pid" 2>/dev/null; then
    worker_pid=""
    return
  fi
  kill -TERM "$worker_pid"
  for _ in $(seq 1 50); do
    if ! kill -0 "$worker_pid" 2>/dev/null; then
      wait "$worker_pid" || true
      worker_pid=""
      return
    fi
    sleep 0.1
  done
  kill -KILL "$worker_pid" 2>/dev/null || true
  wait "$worker_pid" 2>/dev/null || true
  worker_pid=""
  echo "wrangler dev did not exit within five seconds" >&2
  return 1
}

cleanup() {
  shutdown_worker || true
  rm -rf "$temp_dir"
}
trap cleanup EXIT

mkdir -p "$state_dir"
cd "$repo_root"
bun run --cwd apps/rendez-key dev -- \
  --port "$port" \
  --var PUBLIC_SIGNAL_CREATE:true \
  --var API_TOKEN:local-signal-test-token \
  --persist-to "$state_dir" >"$worker_log" 2>&1 &
worker_pid=$!

ready=0
for _ in $(seq 1 100); do
  if curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null 2>&1; then
    ready=1
    break
  fi
  if ! kill -0 "$worker_pid" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if [[ "$ready" != "1" ]]; then
  sed -n '1,240p' "$worker_log" >&2
  echo "wrangler dev did not become healthy" >&2
  exit 1
fi

SIGNAL_SMOKE_ITERATIONS="${SIGNAL_SMOKE_ITERATIONS:-10}" \
SIGNAL_SMOKE_SENTINELS="$sentinels" \
RENDEZKEY_TOKEN="local-signal-test-token" \
  bun apps/rendez-key/scripts/signal-smoke-test.mjs \
    "http://127.0.0.1:$port/v1/signal"

shutdown_worker

while IFS= read -r sentinel; do
  [[ -z "$sentinel" ]] && continue
  if grep -Fq "$sentinel" "$worker_log"; then
    echo "sensitive signaling fixture leaked into workerd logs" >&2
    exit 1
  fi
done <"$sentinels"

echo "wrangler/workerd signaling smoke passed with redaction scan"
