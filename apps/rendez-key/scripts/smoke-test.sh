#!/usr/bin/env bash
set -euo pipefail

: "${BASE_URL:?BASE_URL is required}"
: "${RENDEZKEY_TOKEN:?RENDEZKEY_TOKEN is required}"

VALUE="iroh-ticket-test-$(date +%s)"

CODE="$(
  curl -fsS -X POST \
    "${BASE_URL}/v1/entries?ttl=60&reads=1" \
    -H "Authorization: Bearer ${RENDEZKEY_TOKEN}" \
    -H "Content-Type: text/plain; charset=utf-8" \
    -H "Accept: text/plain" \
    --data-binary "${VALUE}"
)"

RESULT="$(
  curl -fsS -X POST \
    "${BASE_URL}/v1/entries/${CODE}/claim"
)"

if [[ "${RESULT}" != "${VALUE}" ]]; then
  echo "Claimed value does not match uploaded value" >&2
  exit 1
fi

SECOND_STATUS="$(
  curl -sS -o /dev/null -w "%{http_code}" \
    -X POST "${BASE_URL}/v1/entries/${CODE}/claim"
)"

if [[ "${SECOND_STATUS}" != "404" ]]; then
  echo "Expected second claim to return 404, got ${SECOND_STATUS}" >&2
  exit 1
fi

echo "RendezKey smoke test passed"

RENDEZKEY_TOKEN="$RENDEZKEY_TOKEN" \
  bun "$(dirname "$0")/signal-smoke-test.mjs" "${BASE_URL}/v1/signal"
