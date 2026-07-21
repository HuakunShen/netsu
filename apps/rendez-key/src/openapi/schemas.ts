import { z } from "zod";

export const healthResponseSchema = z.object({
  status: z.literal("ok"),
  service: z.literal("rendezkey"),
});

export const createEntryJsonResponseSchema = z.object({
  code: z.string().describe("Short code, formatted XXXX-XXXX"),
  expires_at: z.string().describe("ISO 8601 expiry timestamp"),
  max_reads: z.number().int().min(1).max(100),
});

export const problemResponseSchema = z.object({
  type: z.string().describe("Problem type URI"),
  title: z.string(),
  status: z.number().int(),
  code: z.string(),
  detail: z.string().optional(),
});

export const createSignalRoomRequestSchema = z.strictObject({
  v: z.literal(1),
  ttl_seconds: z.number().int().min(60).max(3_600).default(600),
});

export const createSignalRoomResponseSchema = z.strictObject({
  v: z.literal(1),
  code: z.string().regex(/^[23456789A-HJ-NP-Z]{4}-[23456789A-HJ-NP-Z]{4}$/),
  listener_secret: z.string().regex(/^[A-Za-z0-9_-]{43}$/),
  expires_at: z.string(),
});
