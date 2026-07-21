# Cross-device TUI

**Added: 2026-07-22**

`netsu tui` is an interactive Rust launcher for two-device tests. One device
hosts a test and publishes a short code; the second device joins with that code.
Both sides display a live dashboard and a final summary.

## Host/join flow

1. The host chooses TCP, UDP, WebSocket, or iroh.
2. It publishes a rendez-key value containing `tag|addr`.
3. The joiner claims the code, derives the transport from `tag`, and connects
   without separately selecting a transport or entering a long iroh ticket.

For socket transports, `addr` is the host's advertised LAN `host:port`; the UI
allows editing it because VPN or tunnel routing can select an unsuitable default.
For iroh, `addr` is a self-describing endpoint ticket. Untagged historical
values remain valid and are interpreted as iroh tickets.

## Feature gating

The cross-device code flow requires the `iroh` feature because rendez-key
publishing and claiming use its dependencies. A `tui`-only build intentionally
keeps only offline loopback labs. Run the complete experience with:

```sh
cargo run --features tui,iroh -- tui
```

With `input-demo`, the menu can hand off to the keyboard/mouse-sharing example.
It leaves the Ratatui alternate screen before global input capture starts, then
returns to the menu after the session ends.

## Related pages

- [Rust Implementation](Rust%20Implementation.md)
- [Multiplexing Lab](Multiplexing%20Lab.md)
