import { cleanupEntries } from "./repositories/entries";

export async function runCleanup(
  env: CloudflareBindings,
  scheduledTimeMs: number,
): Promise<void> {
  const nowSeconds = Math.floor(scheduledTimeMs / 1000);
  const deleted = await cleanupEntries(env.DB, nowSeconds);

  console.log(
    JSON.stringify({
      event: "cleanup_completed",
      deleted_rows: deleted,
      status: "ok",
    }),
  );
}
