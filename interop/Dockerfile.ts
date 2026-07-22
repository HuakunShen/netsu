# netsu-ts container: the oven/bun image running the TypeScript CLI from
# source. Build context is the repo root (see docker-compose.yml).
FROM oven/bun:1-alpine

WORKDIR /app

# Dependency layer first, so a source edit doesn't trigger a reinstall.
COPY package.json bun.lockb ./
COPY packages/netsu/package.json ./packages/netsu/
COPY apps/rendez-key/package.json ./apps/rendez-key/
COPY interop/transports/browser/package.json ./interop/transports/browser/
# Preserve the complete lockfile workspace graph, but install only the package
# this image builds. See Bun's path-based workspace filter contract.
RUN bun install --frozen-lockfile --filter ./packages/netsu

COPY packages/netsu ./packages/netsu
WORKDIR /app/packages/netsu
RUN bun run build

# iperf3 is not needed here — this container only runs netsu itself.
ENTRYPOINT ["bun", "dist/cli.mjs"]
