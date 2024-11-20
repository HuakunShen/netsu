import { $ } from "bun";

await $`bun tsup`;
await Bun.build({
  entrypoints: ["./cli.ts"],
  outdir: "./dist",
  format: "esm",
  target: "node",
  splitting: false,
});
