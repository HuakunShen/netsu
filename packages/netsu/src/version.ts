import pkg from "../package.json";

/**
 * Single source of truth for the package version.
 *
 * Read from package.json at build/import time (via TypeScript's
 * `resolveJsonModule`) rather than duplicated as a string literal — the
 * previous code had "netsu-0.2.0" hardcoded in `protocol/params.ts` (the wire
 * `client_version` field) and `cli.ts` (JSON output), independently of
 * package.json's own `version` field, guaranteed to drift on the first
 * release bump.
 *
 * tsdown/rolldown inline JSON imports as a literal object at build time, so
 * the built ESM bundle carries the version as a plain string with no runtime
 * `fs` access or JSON parsing — this keeps working in the published
 * package, not just under a dev runtime.
 */
export const VERSION: string = pkg.version;
