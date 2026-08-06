# netsu server prints the matching client command

Date: 2026-08-06

## Problem

`netsu server --iroh` prints a code/ticket but never tells the user how to
dial it. Unfamiliar users don't remember that the iroh transport requires
`netsu client --iroh <code>` (or `--ws`, `--quic`, `--webrtc` for the other
transports). The server knows everything needed to build the matching client
command except its own reachable address.

## Design

After the existing startup lines, print a `client:` line with a copy-pasteable
`netsu client` command. Label style matches the existing lowercase labels
(`code:`, `ticket:`). One command per line; with multiple active IPs, print one
command per IP.

### Per-transport output

```
# tcp, two active IPs
netsu server listening on 5201 (tcp)
client: netsu client 192.168.1.5 -p 5201
        netsu client 10.0.0.7 -p 5201

# ws
client: netsu client <ip> -p 5201 --ws

# quic self-signed
client: netsu client <ip> -p 5201 --quic --quic-insecure

# quic with --quic-cert/--quic-key
client: netsu client <ip> -p 5201 --quic --quic-ca <cert-path>

# iroh with rendez-key code
netsu server listening (iroh)
code:   SAZN-KKVH   (share this — expires in ~60m)
ticket: <ticket>
client: netsu client --iroh SAZN-KKVH

# webrtc
netsu server listening (webrtc)
code: <code>
client: netsu client --webrtc <code> --signal-url <url>
```

### Edge cases

- **iroh without a code** (rendez-key publish failed or `--no-rendezkey`):
  `client: netsu client --iroh <full-ticket>` — the client already accepts a
  full ticket as HOST.
- **No active IP found** (offline / no route): print
  `client: netsu client <server-ip> -p 5201` as a fill-in placeholder.
- **QUIC `--quic-ca`**: prints the server's cert path as a hint; the operator
  still must copy the CA file to the client machine.
- **IPv4 only**: matches the existing `local_ipv4()` behavior; IPv6 and the
  TUI's `default_advertise_host_with` logic are out of scope.
- **`netsu mux listen`**: out of scope; keeps printing code + ticket only.

## Implementation

- Add the `if-addrs` crate (zero-dependency) as a regular dependency.
- New helper `active_ipv4s() -> Vec<String>` in `src/main.rs`: enumerate
  interfaces, keep up + non-loopback IPv4 addresses. Pure CLI concern, so it
  works in `--no-default-features` builds where `p2p` is feature-gated off.
- In `run_server`'s final match on `server.endpoint_ticket`:
  - `Some(code) if a.webrtc` → append `client: netsu client --webrtc <code>
    --signal-url <url>`
  - `Some(ticket)` (iroh) → append `client: netsu client --iroh
    <code-or-ticket>` (code when one was published, else the ticket)
  - `None` (tcp/ws/quic) → append one `client:` line per active IP with the
    transport flag (`--ws`/`--quic`/none) and the quic trust flag
    (`--quic-insecure` for self-signed, `--quic-ca <path>` for cert/key).

## Testing

- Unit test `active_ipv4s()` returns at least one non-loopback entry when a
  network route exists; formatting of the command lines is covered by a small
  pure string-builder function with table-driven tests per transport.
- Manual: run `netsu server` for each transport and verify the printed
  command dials successfully.
