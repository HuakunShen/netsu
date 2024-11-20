import { defineConfig } from "tsup";

export default defineConfig({
  entry: ["./src/speed-test.ts"],
  dts: true,
  format: ["cjs", "esm"],
  clean: true,
});
