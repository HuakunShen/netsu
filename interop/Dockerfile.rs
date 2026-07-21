# netsu-rs container. Built in two stages so the runtime image carries no Rust
# toolchain, and so no host cross-compile is needed: the build stage is an
# alpine-rust image whose native target is the host arch's musl
# (aarch64-unknown-linux-musl on Apple Silicon, x86_64 on CI) — the binary is
# compiled natively for the container arch, no emulation. Both stages are alpine,
# so the binary's musl link resolves against the runtime image's musl (no
# `+crt-static`, which would break proc-macro crates like async-trait/serde).
#
# Build context is the repo root (see docker-compose.yml), so paths are
# repo-relative. netsu-rs/target is excluded via .dockerignore.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY netsu-rs/Cargo.toml netsu-rs/Cargo.lock ./
COPY netsu-rs/src ./src
# The manifest declares `[[example]] kbm-demo` (feature-gated), so cargo needs
# the example sources present to even parse Cargo.toml — copy them though they
# are not built here.
COPY netsu-rs/examples ./examples
# The interop matrix exercises the WebSocket transport (`--ws`), which is now an
# opt-in feature, so build it in. (iroh is not in the interop matrix.)
RUN cargo build --release --features ws --bin netsu

FROM alpine:3.20
COPY --from=build /src/target/release/netsu /usr/local/bin/netsu
ENTRYPOINT ["/usr/local/bin/netsu"]
