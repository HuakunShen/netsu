import { fileURLToPath } from "node:url";
import {
  cloudflareTest,
  readD1Migrations,
} from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

// API_TOKEN is declared as a required secret (for stable type generation).
// Seed it here so wrangler's config-load validation is satisfied during tests
// and does not warn; the worker itself receives API_TOKEN via miniflare below.
process.env.API_TOKEN ??= "test-token";

export default defineConfig(async () => {
  const migrationsPath = fileURLToPath(
    new URL("./migrations", import.meta.url),
  );
  const migrations = await readD1Migrations(migrationsPath);

  return {
    plugins: [
      cloudflareTest({
        wrangler: {
          configPath: "./wrangler.jsonc",
        },
        miniflare: {
          bindings: {
            API_TOKEN: "test-token",
            TEST_MIGRATIONS: migrations,
          },
        },
      }),
    ],
    test: {
      // Each Cloudflare pool runner starts a workerd process. Starting one per
      // file in parallel can exceed Vitest's fixed 90s worker-start deadline on
      // developer machines and CI. D1 tests also share migration setup, so run
      // files serially while retaining concurrency inside each file.
      fileParallelism: false,
      setupFiles: ["./test/apply-migrations.ts"],
    },
  };
});
