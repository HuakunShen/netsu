#!/usr/bin/env bash
# Run the mux docker-compose experiment once per named netem profile from
# mux-docker/netem-profiles.json. Results land in mux-docker/results/<profile>/.
# Requires docker + jq. Linux (or Docker's Linux VM) for tc/netem.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
compose_dir="$here/mux-docker"
profiles_json="$compose_dir/netem-profiles.json"

PROFILES="${PROFILES:-baseline constrained slow long-haul lossy}"

for profile in $PROFILES; do
  echo "=== netem profile: $profile ==="
  read -r rate delay jitter loss reorder limit < <(
    jq -r --arg p "$profile" \
      '.profiles[$p] | "\(.rate) \(.delay) \(.jitter) \(.loss) \(.reorder) \(.limit)"' \
      "$profiles_json"
  )
  outdir="$compose_dir/results/$profile"
  mkdir -p "$outdir"

  NETEM_ENABLED=1 NETEM_RATE="$rate" NETEM_DELAY="$delay" NETEM_JITTER="$jitter" \
  NETEM_LOSS="$loss" NETEM_REORDER="$reorder" NETEM_LIMIT="$limit" \
  COMPOSE_PROJECT_NAME="netsu_mux_${profile//-/_}" \
    docker compose -f "$compose_dir/docker-compose.yml" up --build --abort-on-container-exit

  cp -f "$compose_dir/results/result.json" "$outdir/result.json" 2>/dev/null || true
  COMPOSE_PROJECT_NAME="netsu_mux_${profile//-/_}" \
    docker compose -f "$compose_dir/docker-compose.yml" down -v || true
done

echo "done — per-profile results under $compose_dir/results/"
