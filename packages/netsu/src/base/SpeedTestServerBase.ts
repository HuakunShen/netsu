import { SpeedTestBase } from "./SpeedTestBase";
import type { SpeedTestServerOptions } from "../types";

export abstract class SpeedTestServerBase extends SpeedTestBase {
  protected readonly options: SpeedTestServerOptions;

  constructor(options: SpeedTestServerOptions) {
    super();
    this.options = options;
  }

  abstract start(): Promise<void>;
  abstract stop(): void;
}
