import { defineConfig } from "tsdown";

export default defineConfig({
  entry: ["src/index.ts", "src/cli.ts"],
  dts: { tsgo: true },
  exports: true,
  platform: "node",
});
