# Extended-transport image used only by the isolated QUIC/WebRTC correctness
# harness. The build context is the repository root.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY netsu-rs/Cargo.toml netsu-rs/Cargo.lock ./
COPY netsu-rs/src ./src
COPY netsu-rs/examples ./examples
RUN cargo build --locked --release --features quic --bin netsu

FROM alpine:3.20
RUN apk add --no-cache bash iproute2 jq procps
COPY --from=build /src/target/release/netsu /usr/local/bin/netsu
COPY interop/transports/netem-entrypoint.sh /usr/local/bin/netem-entrypoint
COPY interop/transports/netem-profiles.json /etc/netsu/netem-profiles.json
RUN chmod +x /usr/local/bin/netem-entrypoint
ENTRYPOINT []
