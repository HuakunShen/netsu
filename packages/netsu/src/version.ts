import pkg from "../package.json" with { type: "json" };

/**
 * Single source of truth for the package version.
 *
 * Read from package.json at build/import time rather than duplicated as a
 * string literal — the previous code had "netsu-0.2.0" hardcoded in
 * `protocol/params.ts` (the wire `client_version` field) and `cli.ts` (JSON
 * output), independently of package.json's own `version` field, guaranteed
 * to drift on the first release bump.
 *
 * The `with { type: "json" }` import attribute (rather than a bare import
 * relying on TypeScript's `resolveJsonModule`) is required for `jsr.json`'s
 * `./src/index.ts` entry point: JSR type-checks straight from source (not
 * the built output), and its resolver rejects a bare `import pkg from
 * "../package.json"` with "Expected a JavaScript or TypeScript module, but
 * identified a Json module" — confirmed via `bunx jsr publish --dry-run
 * --allow-slow-types`. The attribute makes the JSON-module intent explicit
 * and satisfies both JSR's resolver and tsc/tsgo.
 *
 * tsdown/rolldown inline JSON imports as a literal object at build time, so
 * the built ESM bundle carries the version as a plain string with no runtime
 * `fs` access or JSON parsing — this keeps working in the published
 * package, not just under a dev runtime.
 */
export const VERSION: string = pkg.version;
