import type { Context } from "hono";
import type { ContentfulStatusCode } from "hono/utils/http-status";

export function problem(
  c: Context,
  status: ContentfulStatusCode,
  code: string,
  title: string,
  detail?: string,
) {
  c.header("Cache-Control", "no-store");

  return c.json(
    {
      type: `https://rendezkey.dev/problems/${code}`,
      title,
      status,
      code,
      ...(detail === undefined ? {} : { detail }),
    },
    status,
    { "Content-Type": "application/problem+json" },
  );
}
