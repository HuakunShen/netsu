import { createApp } from "./app";
import { runCleanup } from "./scheduled";

export { SignalRoom } from "./signal/room";

const { app } = createApp();

export default {
  fetch: app.fetch,
  scheduled(
    controller: ScheduledController,
    env: CloudflareBindings,
    ctx: ExecutionContext,
  ): void {
    ctx.waitUntil(runCleanup(env, controller.scheduledTime));
  },
} satisfies ExportedHandler<CloudflareBindings>;
