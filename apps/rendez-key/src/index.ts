import { createApp } from "./app";
import { runCleanup } from "./scheduled";

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
