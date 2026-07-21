# Chromium WebRTC peer

This package is an independent browser implementation of signaling-v1 and the
netsu control/result protocol. It imports no Rust-generated protocol code.

The image and JavaScript dependency are both pinned to Playwright 1.61.1. Run
the isolated Chromium protocol tests with:

```sh
docker build -f interop/transports/browser/Dockerfile \
  -t netsu-webrtc-browser:test interop/transports/browser
docker run --rm --init --ipc=host netsu-webrtc-browser:test
```

`run-browser-peer.mjs` is the Compose-facing entry point. Pass `--signal-url`,
`--code`, `--duration`, `--parallel`, optional `--reverse`, repeatable `--stun`,
and optional `--include-addresses`. It prints one JSON result. Direct-path
failure exits 4 and never emits throughput fields.
