import { describe, expect, it } from "vitest";
import corpus from "./fixtures/signal-v1.json";
import {
  SIGNAL_ERROR_CODES,
  parseClientSignalMessage,
  parseSignalFrame,
} from "../src/signal/protocol";
import {
  MAX_SIGNAL_FRAME_BYTES,
  SIGNAL_LISTENER_SECRET_LENGTH,
} from "../src/signal/limits";

describe("signaling v1 protocol", () => {
  it("accepts every golden client message", () => {
    for (const entry of corpus.valid_client_messages) {
      expect(parseClientSignalMessage(entry.message)).toEqual(entry.message);
    }
  });

  it("rejects every golden invalid message with a stable code", () => {
    for (const entry of corpus.invalid_client_messages) {
      expect(() => parseClientSignalMessage(entry.message)).toThrowError(
        entry.error,
      );
    }
  });

  it("rejects wrong versions, types, roles, SDP kinds, and extra fields", () => {
    for (const message of [
      { v: 2, type: "leave" },
      { v: 1, type: "mystery" },
      { v: 1, type: "bind", role: "observer" },
      { v: 1, type: "description", sdp_type: "pranswer", sdp: "fixture" },
      { v: 1, type: "leave", extra: true },
    ]) {
      expect(() => parseClientSignalMessage(message)).toThrowError(
        "invalid_message",
      );
    }
  });

  it("rejects malformed candidate indices and oversized candidate strings", () => {
    const candidate = {
      v: 1,
      type: "candidate",
      candidate: "fixture-candidate",
      sdp_mid: "0",
      username_fragment: "fixture-fragment",
    };
    for (const sdp_mline_index of [-1, 65_536, 1.5, "0"]) {
      expect(() =>
        parseClientSignalMessage({ ...candidate, sdp_mline_index }),
      ).toThrowError("invalid_message");
    }
    expect(() =>
      parseClientSignalMessage({
        ...candidate,
        sdp_mline_index: 0,
        candidate: "x".repeat(4_097),
      }),
    ).toThrowError("invalid_message");
  });

  it("rejects binary frames and frames larger than 64 KiB before JSON parsing", () => {
    expect(() =>
      parseSignalFrame(new Uint8Array([1, 2, 3]).buffer),
    ).toThrowError("invalid_message");
    expect(() =>
      parseSignalFrame("x".repeat(MAX_SIGNAL_FRAME_BYTES + 1)),
    ).toThrowError("resource_limit");
  });

  it("requires an unpadded 256-bit base64url listener secret", () => {
    expect(SIGNAL_LISTENER_SECRET_LENGTH).toBe(43);
    for (const secret of [
      "short",
      "=".repeat(43),
      "a".repeat(42),
      "a".repeat(44),
    ]) {
      expect(() =>
        parseClientSignalMessage({
          v: 1,
          type: "bind",
          role: "listener",
          secret,
        }),
      ).toThrowError("invalid_message");
    }
  });

  it("freezes every public error code", () => {
    expect(SIGNAL_ERROR_CODES).toEqual([
      "invalid_message",
      "room_not_found",
      "room_expired",
      "room_full",
      "unauthorized_listener",
      "unexpected_message",
      "resource_limit",
      "internal_error",
    ]);
  });
});
