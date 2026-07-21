# RendezKey Public Repository Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the complete, currently unused CrossCopy `apps/rendez-key`
Cloudflare Worker into the public netsu repository without changing its
short-code API, while removing private-repository coupling and documenting the
retained production deployment metadata.

**Architecture:** netsu owns `apps/rendez-key` as its network-test control-plane
Worker. It retains the D1-backed temporary string/short-code service and later
adds Durable-Object WebRTC signaling under a separate route/state model. Bun is
the netsu workspace package manager; Wrangler/workerd remains the server runtime.

**Tech Stack:** Bun workspace, Hono, Cloudflare Workers, D1, Drizzle, Wrangler,
Vitest with `@cloudflare/vitest-pool-workers`, TypeScript, shell smoke tests.

## Global Constraints

- Preserve `/healthz`, `/v1/entries`, `/v1/entries/:code/claim`, OpenAPI, and
  typed `AppType` behavior.
- Do not migrate CrossCopy credentials, `.dev.vars`, local Wrangler state,
  `node_modules`, or build outputs. The public custom domain and D1 resource ID
  may remain because they are non-secret deployment identifiers already used by
  netsu; forks must replace them before deploying.
- Do not modify `netsu-rs/src/tui.rs` or unrelated CrossCopy dirty files.
- Keep anonymous and privileged creation tiers distinct.
- Anonymous limits are TTL/read/payload caps plus 10 creates per 60 seconds per
  IP per Cloudflare location; they are not a five-tests-per-hour quota.
- Local and PR automation inject a local test token and bypass anonymous create
  limiting. Public smoke uses a protected CI secret.
- The Worker never carries benchmark payload, proxy traffic, or TURN traffic.
- Verify the migrated copy before deleting the source package.

---

### Task 1: Capture the source behavior baseline

**Files:**

- Read: `<former-crosscopy-checkout>/apps/rendez-key/**`
- Read: `<former-crosscopy-checkout>/apps/rendez-key/test/**`

**Interfaces:**

- Consumes: existing Hono routes, D1 migration, and Worker bindings.
- Produces: recorded typecheck/test/deploy-dry results to compare after move.

- [x] Run `tsc --noEmit` from the source package.
- [x] Run its Vitest Cloudflare pool with an explicit verbose reporter.
- [x] Run `wrangler deploy --dry-run` and record bindings.
- [x] Confirm `git status --short apps/rendez-key` is empty.

### Task 2: Migrate only tracked source into netsu

**Files:**

- Create: `apps/rendez-key/**`
- Modify: `package.json`
- Modify: `bun.lockb`

**Interfaces:**

- Consumes: the exact tracked tree at CrossCopy HEAD.
- Produces: a netsu Bun workspace package named `rendez-key`.

- [x] Export only tracked files with `git archive`; exclude all ignored state.
- [x] Extract the archive as `apps/rendez-key` in netsu.
- [x] Confirm source and destination file manifests match before adaptation.
- [x] Add root scripts `signal:dev`, `signal:test`, `signal:typecheck`, and
      `signal:deploy:dry` that delegate to the workspace package.
- [x] Run `bun install` to update the workspace lock deterministically.

### Task 3: Remove CrossCopy-specific development assumptions

**Files:**

- Modify: `apps/rendez-key/wrangler.jsonc`
- Modify: `apps/rendez-key/package.json`
- Modify: `apps/rendez-key/README.md`
- Modify: `apps/rendez-key/rendezkey-prd.md`
- Modify: `apps/rendez-key/rendezkey-implementation-plan.md`
- Modify: `apps/rendez-key/scripts/build-types.sh`

**Interfaces:**

- Consumes: existing API and Worker runtime.
- Produces: public, fork-deployable configuration with no CrossCopy dependency.

- [x] Keep the worker name, bindings, migrations, current public custom domain,
      and D1 database ID so the existing netsu endpoint remains deployable from its
      new source owner. Document that resource IDs are public identifiers, not
      credentials, and forks must replace them.
- [x] Keep `PUBLIC_CREATE=false` as the fail-closed default.
- [x] Change workspace instructions from pnpm/CrossCopy to Bun/netsu.
- [x] Describe the service as netsu's temporary network-test control plane.
- [x] Document Cloudflare rate limiting as per-location abuse dampening, not an
      exact global quota; document kill switch and budget alert requirements.
- [x] Scan the public tree for CrossCopy paths, private endpoints, secrets,
      tokens, non-placeholder resource IDs, and private package imports.

### Task 4: Verify behavior in the netsu repository

**Files:**

- Test: `apps/rendez-key/test/**`
- Test: existing netsu Rust and TypeScript suites.

**Interfaces:**

- Consumes: migrated Worker package and Bun workspace dependencies.
- Produces: evidence that move and public-config cleanup did not change API behavior.

- [x] Generate Worker binding types and inspect the diff.
- [x] Run `bun run --cwd apps/rendez-key typecheck`.
- [x] Run `bun run --cwd apps/rendez-key test` with explicit test counts.
- [x] Run `bun run --cwd apps/rendez-key deploy:dry` and verify D1, rate-limit,
      cron, observability, and the intentionally retained custom domain.
- [x] Run the existing netsu TypeScript tests and Rust targeted rendez-key tests.
- [x] Run `git diff --check` and secret/resource scans.

### Task 5: Repoint specs, skills, and consumers

**Files:**

- Modify: `docs/specs/2026-07-22-quic-webrtc-transports.md`
- Modify: `docs/plans/2026-07-22-cloudflare-webrtc-signaling.md`
- Modify: `docs/plans/2026-07-22-webrtc-implementation.md`
- Modify: `.agents/skills/rendez-key/SKILL.md`
- Modify: `README.md`
- Modify: `netsu-rs/README.md`
- Modify: `netsu-rs/src/p2p/rendezkey.rs` only if its default endpoint is stale.

**Interfaces:**

- Consumes: `apps/rendez-key` as the sole server source.
- Produces: a self-contained netsu development, E2E, and deployment story.

- [x] Remove all `NETSU_CROSSCOPY_DIR`, CrossCopy checkout, and pinned private
      commit requirements.
- [x] Make local/container CI start `apps/rendez-key` directly with Wrangler.
- [x] Keep production endpoint configurable; do not hard-code a new endpoint
      before it is deployed and smoke-tested.
- [x] Update the rendez-key skill to name netsu as the source repository and Bun
      as the workspace package manager.

### Task 6: Delete the verified source package from CrossCopy

**Files:**

- Delete: `<former-crosscopy-checkout>/apps/rendez-key/**`

**Interfaces:**

- Consumes: successful Task 4 verification and clean manifest comparison.
- Produces: one authoritative public source tree in netsu.

- [x] Search CrossCopy for imports, scripts, CI filters, docs, and package lists
      that still require `apps/rendez-key`.
- [x] Remove or repoint only live references; leave unrelated historical design
      evidence intact unless it claims the current source location.
- [x] Delete the tracked package and its repository-local developer skill.
- [x] Verify CrossCopy has no build dependency on it and inspect the scoped diff.
- [x] Do not stage or alter unrelated dirty files.

## Acceptance checklist

- [x] netsu alone can install, typecheck, test, and run the Worker locally.
- [x] No CrossCopy checkout or private artifact is needed by netsu CI/E2E.
- [x] Existing RendezKey route behavior and concurrency tests pass after move.
- [x] Public config contains no secret; retained domain/D1 identifiers are
      explicitly documented as replace-on-fork production metadata.
- [x] Anonymous automation misunderstanding is documented; the signaling/CI
      plan uses local Worker tests or a protected token rather than anonymous
      public creates.
- [x] CrossCopy contains no live source/build dependency after deletion.
- [x] Existing dirty changes in both repositories remain byte-for-byte untouched.

## Execution record

- Source baseline: typecheck and deploy dry-run passed. The original 11-file
  Worker suite exposed a Vitest/workerd startup timeout when all pool files
  launched concurrently; one isolated file passed.
- Migrated suite: `fileParallelism: false` starts one workerd runner at a time;
  11 files and 35 tests passed in 7.53 seconds.
- Generated bindings list D1, `CREATE_LIMITER` at 10 requests/60s,
  `PUBLIC_CREATE=false`, and the privileged secret without embedding its value.
- Local `bun run signal:dev -- --port 18787` returned `200` from `/healthz` and
  shut down cleanly.
- The old untracked `.dev.vars` was not copied or deleted. It was preserved in
  CrossCopy's ignored `.wrangler/rendez-key-migration-backup/.dev.vars`; only
  regenerable old `node_modules` and Wrangler state were removed.
- CrossCopy's lockfile importer and now-unused package-only dependencies were
  pruned offline. A frozen whole-workspace validation is masked locally by the
  pre-existing ignored `references/netsu` symlink, which pnpm treats as an
  additional workspace; the tracked lockfile diff itself is scoped to removed
  RendezKey dependencies.
