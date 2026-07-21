#!/usr/bin/env sh
# Apply one checked profile to this container's egress, then exec the client.
set -eu

profile="${1:-}"
if [ -z "$profile" ]; then
  echo "netem-entrypoint: missing profile" >&2
  exit 64
fi
shift
if [ "${1:-}" != "--" ]; then
  echo "netem-entrypoint: expected -- before command" >&2
  exit 64
fi
shift
if [ "$#" -eq 0 ]; then
  echo "netem-entrypoint: missing command" >&2
  exit 64
fi

profiles_file="${NETSU_NETEM_PROFILES:-/etc/netsu/netem-profiles.json}"
if ! jq -e --arg profile "$profile" 'has($profile)' "$profiles_file" >/dev/null; then
  echo "netem-entrypoint: unknown profile: $profile" >&2
  exit 64
fi

rate="$(jq -er --arg profile "$profile" '.[$profile].rate' "$profiles_file")"
delay="$(jq -er --arg profile "$profile" '.[$profile].delay' "$profiles_file")"
jitter="$(jq -er --arg profile "$profile" '.[$profile].jitter' "$profiles_file")"
loss="$(jq -er --arg profile "$profile" '.[$profile].loss' "$profiles_file")"

check_value() {
  value="$1"
  pattern="$2"
  if ! printf '%s\n' "$value" | grep -Eq "$pattern"; then
    echo "netem-entrypoint: invalid profile value: $value" >&2
    exit 64
  fi
}

check_value "$rate" '^[0-9]+([.][0-9]+)?(kbit|mbit|gbit)$'
check_value "$delay" '^[0-9]+([.][0-9]+)?(us|ms|s)$'
check_value "$jitter" '^[0-9]+([.][0-9]+)?(us|ms|s)$'
check_value "$loss" '^([0-9]|[1-9][0-9]|100)([.][0-9]+)?%$'

tc qdisc replace dev eth0 root netem \
  rate "$rate" \
  delay "$delay" "$jitter" \
  loss "$loss"

echo "netem-entrypoint: applied profile=$profile rate=$rate delay=$delay jitter=$jitter loss=$loss" >&2
exec "$@"
