import type { Context } from "hono";

export function health(c: Context) {
  return c.json(
    {
      status: "ok",
      service: "rendezkey",
    },
    200,
  );
}
