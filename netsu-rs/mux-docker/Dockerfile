# Build netsu with the iroh feature, then run it under tc/netem. Build context
# is the netsu-rs crate root: `docker build -f mux-docker/Dockerfile .`
FROM rust:1-bookworm AS build
WORKDIR /app
COPY . .
RUN cargo build --release --features iroh --bin netsu

FROM debian:bookworm-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends iproute2 ca-certificates \
  && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/netsu /usr/local/bin/netsu
COPY mux-docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
