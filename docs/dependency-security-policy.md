# Dependency security policy

Last reviewed: 2026-07-22

## Acceptance gate

Before adding a direct Rust or JavaScript dependency:

1. Resolve the repository from crates.io/npm metadata and verify through the
   GitHub API that it is neither archived nor disabled.
2. Check that the selected version is maintained and compatible; a version
   bump is not an acceptable response to an unmaintained advisory when the
   advisory has no patched release.
3. Scan the complete committed lockfile, including transitive dependencies:
   `cargo audit --deny warnings` for Rust and `bun audit --audit-level=low` for
   JavaScript.
4. Record and justify any existing exception. New direct dependencies must not
   add an exception.
5. Run the affected unit, integration, workerd/container, and E2E tests after
   dependency resolution changes.

The CI workflow enforces both lockfile scans. This detects published advisories
and RustSec maintenance notices; it cannot prove the absence of undisclosed or
future vulnerabilities, so the audit must be repeated as advisory databases
change.

## 2026-07-22 QUIC/WebRTC audit

The repositories for the direct dependencies added by the QUIC, WebRTC, and
rendez-key work were checked with the GitHub API. Every repository returned
`archived: false` and `disabled: false`:

- Rust: `quinn-rs/quinn`, `rustls/rustls`, `rustls/pki-types`,
  `rustls/rcgen`, `RustCrypto/hashes`, `webrtc-rs/webrtc` (including
  `webrtc-sctp`), `marshallpierce/rust-base64`, `iqlusioninc/crates`,
  `servo/rust-url`, `tokio-rs/bytes`, `rust-lang/futures-rs`,
  `seanmonstar/reqwest`, and `snapview/tokio-tungstenite`.
- JavaScript: `cloudflare/workers-sdk`, `honojs/hono`, `honojs/middleware`,
  `scalar/scalar`, `DefinitelyTyped/DefinitelyTyped`,
  `drizzle-team/drizzle-orm`, `rhinobase/hono-openapi`,
  `microsoft/TypeScript`, `vitest-dev/vitest`, and `colinhacks/zod`.

`rustls-pemfile` was removed. `RUSTSEC-2025-0134` marks all releases
unmaintained and its repository is archived, so QUIC PEM parsing now uses the
`rustls::pki_types::pem::PemObject` API already re-exported by rustls.

The Worker package no longer depends on `drizzle-kit`: the generator was not
needed at runtime and brought in deprecated `@esbuild-kit/esm-loader` plus an
advisory-affected `esbuild 0.18.x`. Existing SQL migrations remain the source
of truth. Miniflare currently pins advisory-affected `sharp 0.34.5`, so the
root lockfile overrides it to `sharp 0.35.3`; the workerd test gate verifies
the supported netsu server paths with that override.

The Rust lockfile contains two pre-existing, target-specific unmaintained
transitive crates from the Iroh dependency graph:

- `RUSTSEC-2023-0089` (`atomic-polyfill`) via `iroh -> postcard -> heapless`.
- `RUSTSEC-2024-0436` (`paste`) via Iroh's Linux netlink/network-watch stack.

They are explicit CI exceptions and were not introduced by QUIC/WebRTC. The
strict audit still fails for every other vulnerability or informational
warning.
