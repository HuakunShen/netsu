# netsu Server Client Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After `netsu server` starts, print a copy-pasteable `netsu client ...` command (with the correct transport flag and address/code) so users never have to remember `--iroh`/`--ws`/`--quic`/`--webrtc`.

**Architecture:** Two pure helpers in `src/main.rs` — `client_command_line()` builds the command string for any transport, and `active_ipv4s()` enumerates up, non-loopback IPv4 addresses via the new `if-addrs` crate. `run_server`'s final match on `server.endpoint_ticket` gains a `client:` print (one line per active IP for tcp/ws/quic; the code or ticket for iroh; code + `--signal-url` for webrtc). `publish_rendezkey_code` returns `Option<String>` so the iroh branch knows the code.

**Tech Stack:** Rust 2024, clap, tokio. New dependency: `if-addrs = "0.14"` (0.15.0 has a broken docs.rs build; pin 0.14.0 via caret).

**Spec:** `docs/specs/2026-08-06-server-client-command.md`

**Verification gate:** `bash scripts/verify.sh` (cargo fmt --check, clippy -D warnings across the feature matrix, cargo test for default/ws+iroh+quic+tui/webrtc). All code must be clippy-clean.

**Repo conventions:** commit messages are conventional (`feat:`, `fix:`, `chore:`). The `#[cfg(test)] mod tests` block lives at the bottom of `src/main.rs` (lines 840-872).

---

### Task 1: Add the `if-addrs` dependency

**Files:**
- Modify: `netsu-rs/Cargo.toml:52`

- [ ] **Step 1: Add the dependency**

In `netsu-rs/Cargo.toml`, in `[dependencies]` (alphabetical position, after the `futures-util` line at line 52), add:

```toml
if-addrs = "0.14"
```

- [ ] **Step 2: Verify it resolves and builds**

Run: `cargo check`
Expected: builds with no errors; `Cargo.lock` gains an `if-addrs` + `windows-sys`/`libc` entry.

- [ ] **Step 3: Commit**

```bash
git add netsu-rs/Cargo.toml netsu-rs/Cargo.lock
git commit -m "chore: add if-addrs dependency for interface enumeration"
```

---

### Task 2: Write the failing tests (TDD red)

**Files:**
- Modify: `netsu-rs/src/main.rs` (tests module, end of file)

- [ ] **Step 1: Add the two tests**

Append inside the existing `#[cfg(test)] mod tests` block in `src/main.rs` (after the `webrtc_setup_failures_have_stable_exit_codes_and_json_kinds` test, before the closing `}` of the module):

```rust
    #[test]
    fn client_command_lines_match_transport() {
        let cases = [
            ("tcp", 5201, "192.168.1.5", None, None, "netsu client 192.168.1.5 -p 5201"),
            (
                "ws",
                5201,
                "192.168.1.5",
                None,
                None,
                "netsu client 192.168.1.5 -p 5201 --ws",
            ),
            (
                "quic",
                5201,
                "192.168.1.5",
                Some("--quic-insecure"),
                None,
                "netsu client 192.168.1.5 -p 5201 --quic --quic-insecure",
            ),
            (
                "quic",
                5201,
                "192.168.1.5",
                Some("--quic-ca /etc/netsu/ca.pem"),
                None,
                "netsu client 192.168.1.5 -p 5201 --quic --quic-ca /etc/netsu/ca.pem",
            ),
            (
                "iroh",
                5201,
                "SAZN-KKVH",
                None,
                None,
                "netsu client --iroh SAZN-KKVH",
            ),
            (
                "webrtc",
                5201,
                "ABCD-1234",
                None,
                Some("https://signal.example.com"),
                "netsu client --webrtc ABCD-1234 --signal-url https://signal.example.com",
            ),
        ];
        for (transport, port, peer, quic_trust, signal_url, expected) in cases {
            assert_eq!(
                client_command_line(transport, port, peer, quic_trust, signal_url),
                expected
            );
        }
    }

    #[test]
    fn active_ipv4s_are_non_loopback() {
        let ips = active_ipv4s();
        if ips.is_empty() {
            return; // offline host — nothing to assert
        }
        for ip in &ips {
            let v4: std::net::Ipv4Addr = ip.parse().expect("active_ipv4s yields IPv4 only");
            assert!(!v4.is_loopback() && !v4.is_unspecified());
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin netsu`
Expected: compile error — `cannot find function 'client_command_line' in this scope` and `cannot find function 'active_ipv4s' in this scope`.

- [ ] **Step 3: Commit (tests in a red state are not committed — but commit the test scaffolding only after Task 3 turns it green; instead just stage nothing)**

No commit in this task; the tests are committed together with the implementation in Task 3.

---

### Task 3: Implement the helpers (TDD green)

**Files:**
- Modify: `netsu-rs/src/main.rs` (insert after `resolve_peer_host`, which ends at line 455, directly before `async fn run_server`)

- [ ] **Step 1: Add the two helper functions**

Insert this code between `resolve_peer_host` (ends line 455) and `async fn run_server` (line 457):

```rust
/// Active (operationally-up, non-loopback) IPv4 addresses on this host, sorted
/// and deduplicated. Used to build client commands when a server is bound to
/// the wildcard address. Empty when no such address exists (e.g. offline).
fn active_ipv4s() -> Vec<String> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut ips: Vec<String> = interfaces
        .iter()
        .filter(|iface| iface.is_oper_up() && !iface.is_loopback())
        .filter_map(|iface| match iface.ip() {
            std::net::IpAddr::V4(v4) if !v4.is_unspecified() => Some(v4.to_string()),
            _ => None,
        })
        .collect();
    ips.sort();
    ips.dedup();
    ips
}

/// A copy-paste `netsu client` command for a server that just started. `peer`
/// is a host/IP for tcp/ws/quic, or a code/ticket for iroh/webrtc.
fn client_command_line(
    transport: &str,
    port: u16,
    peer: &str,
    quic_trust: Option<&str>,
    signal_url: Option<&str>,
) -> String {
    let args = match transport {
        "iroh" => format!("--iroh {peer}"),
        "webrtc" => format!(
            "--webrtc {peer} --signal-url {}",
            signal_url.unwrap_or("<url>")
        ),
        "ws" => format!("{peer} -p {port} --ws"),
        "quic" => match quic_trust {
            Some(trust) => format!("{peer} -p {port} --quic {trust}"),
            None => format!("{peer} -p {port} --quic"),
        },
        _ => format!("{peer} -p {port}"),
    };
    format!("netsu client {args}")
}
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test --bin netsu`
Expected: PASS — both `client_command_lines_match_transport` and `active_ipv4s_are_non_loopback` green (the second is lenient if the host is offline).

- [ ] **Step 3: Commit**

```bash
git add netsu-rs/src/main.rs
git commit -m "feat: build client command lines and enumerate active IPv4s"
```

---

### Task 4: Print the client command for iroh and webrtc

**Files:**
- Modify: `netsu-rs/src/main.rs:409-437` (`publish_rendezkey_code`)
- Modify: `netsu-rs/src/main.rs:537-551` (first two `match` arms in `run_server`)

- [ ] **Step 1: Change `publish_rendezkey_code` to return the code**

Replace the whole function (currently lines 409-437) with:

```rust
/// Publish the iroh ticket as a short rendez-key code (best-effort — a failure
/// just falls back to the printed ticket). Returns the published code so the
/// caller can build the client command. Works anonymously in open mode; a
/// token (if set) unlocks the privileged tier.
#[cfg(feature = "iroh")]
async fn publish_rendezkey_code(ticket: &str, a: &ServerArgs) -> Option<String> {
    use netsu::p2p::rendezkey;
    let url = a
        .rendezkey_url
        .as_deref()
        .unwrap_or(rendezkey::DEFAULT_BASE_URL);
    let token = rendezkey::token_from_env();
    match rendezkey::store(
        url,
        token.as_deref(),
        ticket,
        a.rendezkey_ttl,
        a.rendezkey_reads,
    )
    .await
    {
        Ok(code) => {
            println!(
                "code:   {code}   (share this — expires in ~{}m)",
                a.rendezkey_ttl / 60
            );
            Some(code)
        }
        Err(e) => {
            eprintln!("netsu server: rendez-key unavailable ({e:#}); share the ticket instead");
            None
        }
    }
}
```

- [ ] **Step 2: Rewrite the webrtc and iroh match arms in `run_server`**

Replace the current `Some(code) if a.webrtc => {...}` and `Some(ticket) => {...}` arms (lines 537-551) with:

```rust
        Some(code) if a.webrtc => {
            println!("netsu server listening (webrtc)");
            println!("code: {code}");
            println!(
                "client: {}",
                client_command_line("webrtc", a.port, code, None, a.signal_url.as_deref())
            );
        }
        // iroh: the client dials this via `--peer`/positional HOST — a short
        // rendez-key code (hand-typable) or the full ticket.
        Some(ticket) => {
            println!("netsu server listening (iroh)");
            #[cfg(feature = "iroh")]
            let rendez_code: Option<String> = if !a.no_rendezkey {
                publish_rendezkey_code(ticket, &a).await
            } else {
                None
            };
            #[cfg(not(feature = "iroh"))]
            let rendez_code: Option<String> = None;
            println!("ticket: {ticket}");
            println!(
                "client: {}",
                client_command_line(
                    "iroh",
                    a.port,
                    rendez_code.as_deref().unwrap_or(ticket),
                    None,
                    None,
                )
            );
        }
```

Note: `let` statements with mutually-exclusive `#[cfg]` attributes are valid Rust; exactly one compiles.

- [ ] **Step 3: Build and run the unit tests**

Run: `cargo test --bin netsu`
Expected: PASS (the two helper tests plus the pre-existing test).

- [ ] **Step 4: Commit**

```bash
git add netsu-rs/src/main.rs
git commit -m "feat: print client command for iroh and webrtc servers"
```

---

### Task 5: Print the client command for tcp/ws/quic (multi-IP + QUIC trust flag)

**Files:**
- Modify: `netsu-rs/src/main.rs:552-565` (`None` arm in `run_server`)

- [ ] **Step 1: Rewrite the `None` match arm**

Replace the current `None => { println!(...) }` arm (lines 552-565) with:

```rust
        None => {
            let transport = if a.ws {
                "ws"
            } else if a.quic {
                "quic"
            } else if a.webrtc {
                "webrtc"
            } else {
                "tcp"
            };
            println!("netsu server listening on {} ({transport})", server.port);
            // QUIC clients need the matching trust flag; --quic-self-signed
            // dials with --quic-insecure, cert/key dials with --quic-ca. The
            // cert path is shell-quoted (helper splices quic_trust verbatim).
            let quic_trust: Option<String> = if a.quic {
                if a.quic_self_signed {
                    Some("--quic-insecure".to_string())
                } else if let Some(cert) = &a.quic_cert {
                    Some(format!("--quic-ca {}", shell_arg(&cert.display().to_string())))
                } else {
                    None
                }
            } else {
                None
            };
            let ips = active_ipv4s();
            let commands: Vec<String> = if ips.is_empty() {
                vec![client_command_line(
                    transport,
                    server.port,
                    "<server-ip>",
                    quic_trust.as_deref(),
                    None,
                )]
            } else {
                ips.iter()
                    .map(|ip| {
                        client_command_line(
                            transport,
                            server.port,
                            ip,
                            quic_trust.as_deref(),
                            None,
                        )
                    })
                    .collect()
            };
            println!("client: {}", commands[0]);
            for line in &commands[1..] {
                println!("        {line}");
            }
        }
```

- [ ] **Step 2: Format, lint, and test**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test --bin netsu`
Expected: fmt clean, clippy clean, tests PASS.

- [ ] **Step 3: Manual smoke — tcp**

In one terminal run: `cargo run -- server`
Expected output ends with, e.g.:

```
netsu server listening on 5201 (tcp)
client: netsu client 192.168.1.5 -p 5201
```

In a second terminal, dial using the printed command plus `-t 2`:
`cargo run -- client 192.168.1.5 -p 5201 -t 2`
Expected: test completes with a summary. Ctrl-C the server.

- [ ] **Step 4: Manual smoke — ws**

Terminal A: `cargo run -- server --ws`
Expected: `client: netsu client <ip> -p 5201 --ws`
Terminal B: `cargo run -- client <ip> -p 5201 --ws -t 2`
Expected: completes. Ctrl-C the server.

- [ ] **Step 5: Manual smoke — quic self-signed**

Terminal A: `cargo run -- server --quic --quic-self-signed`
Expected: `client: netsu client <ip> -p 5201 --quic --quic-insecure`
Terminal B: `cargo run -- client <ip> -p 5201 --quic --quic-insecure -t 2`
Expected: completes. Ctrl-C the server.

- [ ] **Step 6: Commit**

```bash
git add netsu-rs/src/main.rs
git commit -m "feat: print client command with all active IPs for tcp/ws/quic servers"
```

---

### Task 6: Full verification gate

- [ ] **Step 1: Run the repo's full gate**

Run: `bash scripts/verify.sh`
Expected: fmt check clean, all four clippy invocations clean, all three test feature-matrix runs pass, release build succeeds, mux local smoke succeeds.

- [ ] **Step 2: If any check fails, fix and re-run until the gate passes**

---

## Self-review notes

- **Spec coverage:** tcp/ws/quic multi-IP listing (Task 5), quic trust flag for self-signed and cert/key (Task 5), iroh code-or-ticket fallback (Task 4), webrtc code + `--signal-url` (Task 4), `<server-ip>` placeholder when offline (Task 5), IPv4-only + mux out of scope (not implemented, per spec).
- **Placeholders:** every step carries full code; manual smokes use concrete commands.
- **Type consistency:** `client_command_line(transport: &str, port: u16, peer: &str, quic_trust: Option<&str>, signal_url: Option<&str>) -> String` and `active_ipv4s() -> Vec<String>` are used identically in Tasks 3-5; `publish_rendezkey_code -> Option<String>` matches its two call sites in Task 4.
