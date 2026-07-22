#!/usr/bin/env bash
# Full verification gate for netsu-rs: formatting, lints, tests, and smokes
# across the feature matrix. Run from anywhere.
set -euo pipefail
cd "$(dirname "$0")/.."

say() { printf '\n\033[1;35m== %s ==\033[0m\n' "$1"; }

say "rustfmt"
cargo fmt --check

say "clippy (default)"
cargo clippy --all-targets -- -D warnings

say "clippy (ws,iroh,quic,tui)"
cargo clippy --all-targets --features ws,iroh,quic,tui -- -D warnings

say "clippy (webrtc)"
cargo clippy --locked --all-targets --features webrtc -- -D warnings

say "clippy (input-demo example)"
cargo clippy --features input-demo --example kbm-demo -- -D warnings

say "test (default — TCP/UDP core)"
cargo test

say "test (ws,iroh,quic,tui)"
cargo test --features ws,iroh,quic,tui

say "test (webrtc)"
cargo test --locked --features webrtc

say "release build (size-optimized)"
cargo build --release
size=$(wc -c < target/release/netsu)
printf 'base binary: %s bytes\n' "$size"

say "mux local smoke"
cargo run --release --features iroh --quiet -- \
  mux local --scenario input-file --duration 1s --warmup 200ms --cooldown 100ms >/dev/null

say "iroh throughput direct smoke"
cargo build --release --features iroh >/dev/null
./target/release/netsu server --iroh --direct-only >/tmp/netsu-verify-srv.log 2>&1 &
srv=$!
sleep 2
ticket=$(grep -o 'ticket: .*' /tmp/netsu-verify-srv.log | sed 's/ticket: //')
./target/release/netsu client "$ticket" --iroh --direct-only -t 1 --json >/dev/null
kill "$srv" 2>/dev/null || true

printf '\n\033[1;32mall checks passed\033[0m\n'
