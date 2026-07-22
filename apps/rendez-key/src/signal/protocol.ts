import { z } from "zod";
import {
  MAX_SIGNAL_CANDIDATE_BYTES,
  MAX_SIGNAL_FRAME_BYTES,
  MAX_SIGNAL_SDP_BYTES,
  SIGNAL_LISTENER_SECRET_BYTES,
  SIGNAL_LISTENER_SECRET_LENGTH,
  SIGNAL_PROTOCOL_VERSION,
} from "./limits";

export const SIGNAL_ERROR_CODES = [
  "invalid_message",
  "room_not_found",
  "room_expired",
  "room_full",
  "unauthorized_listener",
  "unexpected_message",
  "resource_limit",
  "internal_error",
] as const;

export type SignalErrorCode = (typeof SIGNAL_ERROR_CODES)[number];

export class SignalProtocolError extends Error {
  constructor(public readonly code: SignalErrorCode) {
    super(code);
    this.name = "SignalProtocolError";
  }
}

const utf8 = new TextEncoder();
const v1 = z.literal(SIGNAL_PROTOCOL_VERSION);

function encodedBytes(value: string): number {
  return utf8.encode(value).byteLength;
}

function validListenerSecret(value: string): boolean {
  if (
    value.length !== SIGNAL_LISTENER_SECRET_LENGTH ||
    !/^[A-Za-z0-9_-]+$/.test(value)
  ) {
    return false;
  }
  try {
    const base64 = value.replace(/-/g, "+").replace(/_/g, "/") + "=";
    return atob(base64).length === SIGNAL_LISTENER_SECRET_BYTES;
  } catch {
    return false;
  }
}

const listenerBindSchema = z.strictObject({
  v: v1,
  type: z.literal("bind"),
  role: z.literal("listener"),
  secret: z.string().refine(validListenerSecret),
});

const joinerBindSchema = z.strictObject({
  v: v1,
  type: z.literal("bind"),
  role: z.literal("joiner"),
});

export const descriptionMessageSchema = z.strictObject({
  v: v1,
  type: z.literal("description"),
  sdp_type: z.enum(["offer", "answer"]),
  sdp: z
    .string()
    .min(1)
    .refine((value) => encodedBytes(value) <= MAX_SIGNAL_SDP_BYTES),
});

export const candidateMessageSchema = z.strictObject({
  v: v1,
  type: z.literal("candidate"),
  candidate: z
    .string()
    .min(1)
    .refine((value) => encodedBytes(value) <= MAX_SIGNAL_CANDIDATE_BYTES),
  sdp_mid: z.string().max(256).nullable(),
  sdp_mline_index: z.number().int().min(0).max(65_535).nullable(),
  username_fragment: z.string().max(256).nullable(),
});

export const clientSignalMessageSchema = z.union([
  listenerBindSchema,
  joinerBindSchema,
  descriptionMessageSchema,
  candidateMessageSchema,
  z.strictObject({ v: v1, type: z.literal("end_of_candidates") }),
  z.strictObject({ v: v1, type: z.literal("leave") }),
]);

const boundMessageSchema = z.strictObject({
  v: v1,
  type: z.literal("bound"),
  role: z.enum(["listener", "joiner"]),
  expires_in_seconds: z.number().int().min(0).max(3_600),
});

export const serverSignalMessageSchema = z.union([
  boundMessageSchema,
  z.strictObject({ v: v1, type: z.literal("peer_ready") }),
  descriptionMessageSchema,
  candidateMessageSchema,
  z.strictObject({ v: v1, type: z.literal("end_of_candidates") }),
  z.strictObject({ v: v1, type: z.literal("peer_left") }),
  z.strictObject({
    v: v1,
    type: z.literal("error"),
    code: z.enum(SIGNAL_ERROR_CODES),
    message: z.string().min(1).max(256),
  }),
]);

export type ClientSignalMessage = z.infer<typeof clientSignalMessageSchema>;
export type ServerSignalMessage = z.infer<typeof serverSignalMessageSchema>;
export type SignalRole = "listener" | "joiner";

export function parseClientSignalMessage(value: unknown): ClientSignalMessage {
  const parsed = clientSignalMessageSchema.safeParse(value);
  if (!parsed.success) {
    throw new SignalProtocolError("invalid_message");
  }
  return parsed.data;
}

export function parseSignalFrame(
  frame: string | ArrayBuffer,
): ClientSignalMessage {
  if (typeof frame !== "string") {
    throw new SignalProtocolError("invalid_message");
  }
  if (encodedBytes(frame) > MAX_SIGNAL_FRAME_BYTES) {
    throw new SignalProtocolError("resource_limit");
  }
  let value: unknown;
  try {
    value = JSON.parse(frame);
  } catch {
    throw new SignalProtocolError("invalid_message");
  }
  return parseClientSignalMessage(value);
}

export function serializeSignalMessage(message: ServerSignalMessage): string {
  return JSON.stringify(serverSignalMessageSchema.parse(message));
}
