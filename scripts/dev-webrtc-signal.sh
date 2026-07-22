#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app_dir="$repo_root/apps/rendez-key"
port="${SIGNAL_WORKER_PORT:-18787}"
temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/netsu-webrtc-signal.XXXXXX")"
worker_log="$temp_dir/workerd.log"
worker_pid=""

cleanup() {
  if [[ -n "$worker_pid" ]] && kill -0 "$worker_pid" 2>/dev/null; then
    kill -TERM "$worker_pid" 2>/dev/null || true
    for _ in $(seq 1 50); do
      kill -0 "$worker_pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -KILL "$worker_pid" 2>/dev/null || true
    wait "$worker_pid" 2>/dev/null || true
  fi
  rm -rf "$temp_dir"
}
trap cleanup EXIT INT TERM

[[ -f "$app_dir/wrangler.jsonc" ]] || {
  echo "missing in-repository apps/rendez-key/wrangler.jsonc" >&2
  exit 1
}

cd "$repo_root"
bun run signal:dev -- \
  --port "$port" \
  --var PUBLIC_SIGNAL_CREATE:true \
  --var API_TOKEN:local-signal-test-token \
  --persist-to "$temp_dir/state" >"$worker_log" 2>&1 &
worker_pid=$!

for _ in $(seq 1 200); do
  if curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null 2>&1; then
    echo "READY http://127.0.0.1:$port/v1/signal"
    wait "$worker_pid"
    exit $?
  fi
  if ! kill -0 "$worker_pid" 2>/dev/null; then
    sed -n '1,240p' "$worker_log" >&2
    exit 1
  fi
  sleep 0.1
done

sed -n '1,240p' "$worker_log" >&2
echo "Wrangler signaling worker did not become healthy" >&2
exit 1
