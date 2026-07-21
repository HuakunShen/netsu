#!/bin/sh
# Apply tc/netem network conditions (if enabled) then run the given command.
# Each NETEM_* value is re-validated here — a plain number-with-unit only — so a
# profile string can never smuggle shell metacharacters into `tc`. Mirrors the
# rules in src/mux/netem.rs. Exit 64 on a bad value.
set -eu

if [ "${NETEM_ENABLED:-0}" = "1" ]; then
  check() {
    echo "$1" | grep -Eq "$2" || {
      echo "entrypoint: invalid netem value: $1" >&2
      exit 64
    }
  }
  RATE="${NETEM_RATE:-100mbit}"
  DELAY="${NETEM_DELAY:-50ms}"
  JITTER="${NETEM_JITTER:-0ms}"
  LOSS="${NETEM_LOSS:-0%}"
  REORDER="${NETEM_REORDER:-0%}"
  LIMIT="${NETEM_LIMIT:-1000}"

  check "$RATE" '^[0-9]+(\.[0-9]+)?(mbit|kbit|gbit)$'
  check "$DELAY" '^[0-9]+(\.[0-9]+)?(ms|us|s)$'
  check "$JITTER" '^[0-9]+(\.[0-9]+)?(ms|us|s)$'
  check "$LOSS" '^[0-9]+(\.[0-9]+)?%$'
  check "$REORDER" '^[0-9]+(\.[0-9]+)?%$'
  check "$LIMIT" '^[0-9]+$'

  tc qdisc replace dev eth0 root netem \
    rate "$RATE" \
    delay "$DELAY" "$JITTER" \
    loss "$LOSS" \
    reorder "$REORDER" \
    limit "$LIMIT"
  echo "entrypoint: applied netem rate=$RATE delay=$DELAY jitter=$JITTER loss=$LOSS reorder=$REORDER" >&2
fi

exec "$@"
