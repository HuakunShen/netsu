# The netsu-rs binary is statically linked against musl, so nothing else is
# required at runtime. Built on the host by interop/build-rust.sh (see
# docker-compose.yml's NETSU_RS_BIN arg, which selects the host-arch binary).
FROM alpine:3.20

ARG NETSU_RS_BIN=interop/bin/netsu-rs-x86_64
COPY ${NETSU_RS_BIN} /usr/local/bin/netsu
RUN chmod +x /usr/local/bin/netsu

ENTRYPOINT ["/usr/local/bin/netsu"]
