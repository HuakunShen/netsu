import { Hono } from "hono";
import { describeRoute, openAPIRouteHandler, resolver } from "hono-openapi";
import { Scalar } from "@scalar/hono-api-reference";
import { claimEntryRoute } from "./routes/claim-entry";
import { createEntryRoute } from "./routes/create-entry";
import { health } from "./routes/health";
import { authorizeCreate, type CreateAuthVariables } from "./http/auth";
import { problem } from "./http/errors";
import {
  createEntryJsonResponseSchema,
  healthResponseSchema,
  problemResponseSchema,
} from "./openapi/schemas";

const problemContent = {
  "application/problem+json": { schema: resolver(problemResponseSchema) },
};

export function createApp() {
  const app = new Hono<{
    Bindings: CloudflareBindings;
    Variables: CreateAuthVariables;
  }>();

  const routes = app
    .use("*", async (c, next) => {
      await next();
      c.header("Cache-Control", "no-store");
    })
    .get(
      "/healthz",
      describeRoute({
        tags: ["Health"],
        summary: "Health check",
        description: "Returns service health. Does not touch D1.",
        responses: {
          200: {
            description: "Service is healthy",
            content: {
              "application/json": { schema: resolver(healthResponseSchema) },
            },
          },
        },
      }),
      health,
    )
    .post(
      "/v1/entries",
      describeRoute({
        tags: ["Entries"],
        summary: "Store a temporary string",
        description:
          "Uploads a UTF-8 string and returns a short human-safe code. A valid " +
          "Bearer API token grants the privileged tier (TTL up to 7 days, up to " +
          "100 reads, 64 KiB). When the deployment enables open mode " +
          "(`PUBLIC_CREATE`), unauthenticated callers are also accepted under a " +
          "tighter, per-IP rate-limited anonymous tier (TTL up to 1 hour, up to " +
          "5 reads, 8 KiB). Send `Accept: text/plain` to receive only the code.",
        security: [{ bearerAuth: [] }, {}],
        parameters: [
          {
            name: "ttl",
            in: "query",
            required: false,
            description:
              "Time-to-live in seconds. Default 3600, range 60-604800.",
            schema: {
              type: "integer",
              minimum: 60,
              maximum: 604_800,
              default: 3600,
            },
          },
          {
            name: "reads",
            in: "query",
            required: false,
            description: "Maximum successful claims. Default 1, range 1-100.",
            schema: { type: "integer", minimum: 1, maximum: 100, default: 1 },
          },
        ],
        requestBody: {
          required: true,
          content: {
            "text/plain": {
              schema: { type: "string", minLength: 1, maxLength: 65_536 },
            },
          },
        },
        responses: {
          201: {
            description: "Entry created",
            content: {
              "application/json": {
                schema: resolver(createEntryJsonResponseSchema),
              },
              "text/plain": { schema: { type: "string" } },
            },
          },
          400: { description: "Invalid request", content: problemContent },
          401: {
            description: "Missing or invalid API token (closed mode)",
            content: problemContent,
          },
          413: { description: "Payload too large", content: problemContent },
          429: {
            description: "Anonymous per-IP rate limit exceeded (open mode)",
            content: problemContent,
          },
          503: {
            description: "Code generation failed",
            content: problemContent,
          },
        },
      }),
      authorizeCreate,
      createEntryRoute,
    )
    .post(
      "/v1/entries/:code/claim",
      describeRoute({
        tags: ["Entries"],
        summary: "Claim a temporary string",
        description:
          "Claims the string stored under a short code. Public: no auth required, the " +
          "code itself is the bearer capability. Invalid, expired, exhausted, or unknown " +
          "codes all return the same 404.",
        parameters: [
          {
            name: "code",
            in: "path",
            required: true,
            description: "8-character short code, with or without a hyphen.",
            schema: { type: "string", example: "7K3M-Q9TX" },
          },
        ],
        responses: {
          200: {
            description: "Claim succeeded; body is the original stored string",
            content: { "text/plain": { schema: { type: "string" } } },
          },
          404: { description: "Entry not available", content: problemContent },
        },
      }),
      claimEntryRoute,
    );

  app.get(
    "/openapi.json",
    openAPIRouteHandler(routes, {
      documentation: {
        info: {
          title: "RendezKey",
          version: "1.0.0",
          description:
            "Store a temporary string, get a short code back; claim it once (or up to " +
            "`reads` times) on another device before it expires.",
        },
        components: {
          securitySchemes: {
            bearerAuth: { type: "http", scheme: "bearer" },
          },
        },
      },
    }),
  );

  app.get("/docs", Scalar({ url: "/openapi.json", theme: "elysiajs", pageTitle: "RendezKey API Docs" }));

  app.notFound((c) => problem(c, 404, "not_found", "Route not found"));

  app.onError((error, c) => {
    console.error(
      JSON.stringify({
        event: "unhandled_error",
        message: error.message,
        status: 500,
      }),
    );

    return problem(c, 500, "internal_error", "Internal server error");
  });

  return { app, routes };
}

export type AppType = ReturnType<typeof createApp>["routes"];
