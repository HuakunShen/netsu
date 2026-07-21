# netsu-ts container: the oven/bun image running the TypeScript CLI from
# source. Build context is the repo root (see docker-compose.yml).
FROM oven/bun:1-alpine

WORKDIR /app

# Dependency layer first, so a source edit doesn't trigger a reinstall.
COPY package.json bun.lockb ./
COPY packages/netsu/package.json ./packages/netsu/
RUN bun install --frozen-lockfile

COPY packages/netsu ./packages/netsu
WORKDIR /app/packages/netsu
RUN bun run build

# iperf3 is not needed here — this container only runs netsu itself.
ENTRYPOINT ["bun", "dist/cli.mjs"]
