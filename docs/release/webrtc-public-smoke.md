# Direct-only WebRTC public-network smoke

This is a manual release gate. It verifies a real public signaling deployment
and a real direct ICE path between two physical networks. It must not run in PR
CI, must not loop against the anonymous room-creation tier, and must not add a
TURN fallback when direct ICE is unavailable.

## What this test proves

- The deployed Cloudflare Worker can create a short-lived room and forward the
  bounded offer/answer/ICE signaling transcript.
- Optional STUN discovery can produce a direct `srflx`/`prflx` path across two
  real networks when their NAT and firewall policies permit it.
- The netsu control protocol and payload DataChannels complete over that direct
  path, with reconciled non-zero application bytes.
- A restrictive network fails within the bounded setup deadline with exit code
  4, a warning, and no fake zero-throughput result.

It does not establish a general availability or performance guarantee for the
external STUN service, every NAT topology, or the public Internet. Container
results and public-network results must be reported separately.

## Roles and data boundary

| Component                       | Responsibility                                          | Sees benchmark payload       |
| ------------------------------- | ------------------------------------------------------- | ---------------------------- |
| RendezKey Worker/Durable Object | Room creation and short-lived signaling                 | No                           |
| STUN server                     | Public-address/NAT mapping discovery                    | No                           |
| WebRTC peers                    | Direct ICE, DTLS/SCTP, control and payload DataChannels | Yes                          |
| TURN server                     | Relay fallback                                          | Unsupported and not deployed |

Google's `stun:stun.l.google.com:19302` is an optional external fixture for
this manual test only. netsu makes no availability, privacy, quota, or pricing
promise for it. A release may instead use another explicitly approved STUN
service; both peers must use the same URL.

## Prerequisites

1. Use the exact release commit on both devices and build the Rust CLI with
   `--features webrtc`.
2. Use two genuinely different networks, for example wired broadband on device
   A and cellular tethering on device B. Two devices on one Wi-Fi network are a
   LAN test, not this smoke test.
3. Keep `PUBLIC_SIGNAL_CREATE=false` for the protected production smoke. Store
   `API_TOKEN` as a Wrangler secret and provide it only to the server process as
   `NETSU_SIGNAL_TOKEN`. Never put it in a URL, command transcript, or artifact.
4. Do not pass `--include-addresses`. Candidate types and ICE protocol are
   enough for release evidence; candidate addresses are private data.

Set non-secret values independently in both terminals:

```bash
export NETSU_SIGNAL_URL="https://rendez-key.xc.huakun.tech/v1/signal"
export NETSU_STUN_URL="stun:stun.l.google.com:19302"
```

## 1. Validate and deploy the Worker

From the release checkout:

```bash
bun install --frozen-lockfile
bun run signal:typecheck
bun run signal:test
bun run signal:test:workerd
bun run signal:deploy:dry
```

Inspect the dry-run output before deploying. It must include `SIGNAL_ROOMS`,
the Durable Object migration, D1, both rate-limit bindings, and closed public
creation switches. Deploy only through the normal protected release process;
the smoke procedure does not grant deployment authority.

After deployment, use the protected token to verify both RendezKey surfaces:

```bash
BASE_URL="${NETSU_SIGNAL_URL%/v1/signal}" \
RENDEZKEY_TOKEN="<redacted API_TOKEN>" \
bun run --cwd apps/rendez-key smoke

curl --fail --silent --show-error \
  "${NETSU_SIGNAL_URL%/v1/signal}/healthz"
```

The smoke must create and finish a signaling room and confirm that the terminal
room cannot be reused. Do not load-test the public anonymous limiter.

## 2. Successful direct-path run

On device A, create one fresh room. Enter the token without saving it in shell
history, then run:

```bash
read -rs NETSU_SIGNAL_TOKEN
export NETSU_SIGNAL_TOKEN

netsu server --webrtc \
  --signal-url "$NETSU_SIGNAL_URL" \
  --stun "$NETSU_STUN_URL"
```

Copy only the printed room code to device B. Do not copy the listener secret or
any SDP/candidate material. On device B:

```bash
set +e
netsu client <ROOM_CODE> --webrtc \
  --signal-url "$NETSU_SIGNAL_URL" \
  --stun "$NETSU_STUN_URL" \
  -t 10 -P 4 --json \
  >webrtc-public-success.json 2>webrtc-public-success.stderr
status=$?
set -e
test "$status" -eq 0
```

Repeat with `-R` using a newly created room. Rooms are single-use.

For each successful run, validate the redacted result:

```bash
jq -e '
  .connection.transport == "webrtc" and
  .connection.path == "direct" and
  (.connection.local_candidate_type as $local |
    ["host", "srflx", "prflx"] | index($local) != null) and
  (.connection.remote_candidate_type as $remote |
    ["host", "srflx", "prflx"] | index($remote) != null) and
  .connection.addresses_included == false and
  .connection.ice_protocol != "unknown" and
  .end.sum_sent.bytes > 0 and
  .end.sum_received.bytes > 0 and
  .end.sum_sent.bits_per_second > 0 and
  .end.sum_sent.bits_per_second < 1e12 and
  .end.sum_received.bits_per_second > 0 and
  .end.sum_received.bits_per_second < 1e12
' webrtc-public-success.json
```

Record the release commit, UTC time, both OS versions, coarse network types
(for example `home broadband` and `cellular`), direction, parallel count,
candidate types, ICE protocol, setup phase timings, byte counts, and throughput.
Do not record IP addresses, listener secrets, SDP, or raw ICE candidates.

## 3. Restrictive-network run

Use a fresh room and a network known to block the required peer-to-peer UDP
path. On device A, rerun the server command from step 2 on that network and
copy its newly printed room code; rooms from the successful run are terminal.
Do not add TURN and do not relax the deadline. On device B, run the client and
capture stdout/stderr:

```bash
set +e
netsu client <NEW_ROOM_CODE> --webrtc \
  --signal-url "$NETSU_SIGNAL_URL" \
  --stun "$NETSU_STUN_URL" \
  -t 10 --json \
  >webrtc-public-blocked.json 2>webrtc-public-blocked.stderr
status=$?
set -e

test "$status" -eq 4
grep -F "warning: WebRTC direct connection failed; netsu does not use TURN relay, so no throughput test was run" \
  webrtc-public-blocked.stderr
jq -e '
  .error.transport == "webrtc" and
  .error.kind == "direct_path_unavailable"
' webrtc-public-blocked.json
! grep -q '"bits_per_second"' webrtc-public-blocked.json
```

A bounded direct-path failure is a passing result for this case. An apparent
success through a `relay` candidate, a timeout beyond the documented setup
bound, or any throughput field in the failure result is a release blocker.

## 4. Artifact review and cleanup

- Search artifacts for `listener_secret`, `"sdp"`, `candidate:`, bearer tokens,
  and candidate addresses before sharing them.
- Keep only the structured client result, stderr warning, environment summary,
  and release commit. Do not upload Worker debug logs unless separately
  reviewed and redacted.
- Unset `NETSU_SIGNAL_TOKEN` on device A after the run.
- Confirm the single-use rooms terminate. No D1 row is created for signaling;
  the Durable Object owns only short-lived room state.

If the successful case fails, first distinguish Worker/signaling failure from
direct ICE failure using the structured error and setup phase. Do not treat a
TURN relay as a repair. Relay measurement, if ever added, requires a separate
transport/path mode, cost controls, abuse controls, and result classification.
