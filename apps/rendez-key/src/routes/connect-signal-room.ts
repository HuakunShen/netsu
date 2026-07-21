import type { Handler } from "hono";
import { normalizeCode } from "../domain/code";
import { problem } from "../http/errors";

export const connectSignalRoomRoute: Handler<{
  Bindings: CloudflareBindings;
}> = async (c) => {
  const code = normalizeCode(c.req.param("code") ?? "");
  if (code === null) {
    return problem(c, 404, "room_not_found", "Room is unavailable");
  }
  const stub = c.env.SIGNAL_ROOMS.getByName(code);
  return stub.fetch(c.req.raw);
};
