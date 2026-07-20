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
RUN cargo build --release --bin netsu

FROM alpine:3.20
COPY --from=build /src/target/release/netsu /usr/local/bin/netsu
ENTRYPOINT ["/usr/local/bin/netsu"]
