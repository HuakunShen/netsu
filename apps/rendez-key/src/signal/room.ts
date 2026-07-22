import { DurableObject } from "cloudflare:workers";
import {
  MAX_SIGNAL_CANDIDATES_PER_PEER,
  MAX_SIGNAL_FORWARDED_BYTES,
  SIGNAL_BIND_DEADLINE_MS,
  SIGNAL_EXPIRED_CLOSE_CODE,
  SIGNAL_INTERNAL_CLOSE_CODE,
  SIGNAL_POLICY_CLOSE_CODE,
  SIGNAL_PROTOCOL_VERSION,
} from "./limits";
import {
  SignalProtocolError,
  type ClientSignalMessage,
  type ServerSignalMessage,
  type SignalErrorCode,
  type SignalRole,
  parseSignalFrame,
  serializeSignalMessage,
} from "./protocol";
import { verifyListenerSecret } from "./secret";

export type SignalRoomLifecycle =
  | "listener-created"
  | "listener-bound"
  | "paired"
  | "closed";

export interface InitializeSignalRoomInput {
  version: typeof SIGNAL_PROTOCOL_VERSION;
  listenerSecretHash: string;
  createdAt: number;
  expiresAt: number;
}

export interface InitializeSignalRoomResult {
  created: boolean;
  matchesInput: boolean;
  lifecycle: SignalRoomLifecycle;
  expiresAt: number;
}

interface SignalRoomRow extends Record<string, SqlStorageValue> {
  version: number;
  lifecycle: SignalRoomLifecycle;
  created_at: number;
  expires_at: number;
  listener_secret_hash: string;
  listener_candidates: number;
  joiner_candidates: number;
  forwarded_bytes: number;
  offer_seen: number;
  answer_seen: number;
  terminal_reason: string | null;
}

interface SignalSocketAttachment {
  version: typeof SIGNAL_PROTOCOL_VERSION;
  sessionId: string;
  connectedAt: number;
  bound: boolean;
  role: SignalRole | null;
}

const utf8 = new TextEncoder();

function signalResponse(status: number, code: SignalErrorCode): Response {
  return Response.json(
    { v: SIGNAL_PROTOCOL_VERSION, error: code },
    { status, headers: { "Cache-Control": "no-store" } },
  );
}

export class SignalRoom extends DurableObject<CloudflareBindings> {
  constructor(ctx: DurableObjectState, env: CloudflareBindings) {
    super(ctx, env);
    ctx.blockConcurrencyWhile(async () => {
      this.ctx.storage.sql.exec(`
        CREATE TABLE IF NOT EXISTS signal_room (
          id INTEGER PRIMARY KEY CHECK (id = 1),
          version INTEGER NOT NULL,
          lifecycle TEXT NOT NULL,
          created_at INTEGER NOT NULL,
          expires_at INTEGER NOT NULL,
          listener_secret_hash TEXT NOT NULL,
          listener_candidates INTEGER NOT NULL DEFAULT 0,
          joiner_candidates INTEGER NOT NULL DEFAULT 0,
          forwarded_bytes INTEGER NOT NULL DEFAULT 0,
          offer_seen INTEGER NOT NULL DEFAULT 0,
          answer_seen INTEGER NOT NULL DEFAULT 0,
          terminal_reason TEXT
        )
      `);
    });
  }

  private row(): SignalRoomRow | null {
    return (
      this.ctx.storage.sql
        .exec<SignalRoomRow>(
          `SELECT version, lifecycle, created_at, expires_at,
                  listener_secret_hash, listener_candidates,
                  joiner_candidates, forwarded_bytes, offer_seen,
                  answer_seen, terminal_reason
             FROM signal_room WHERE id = 1`,
        )
        .toArray()[0] ?? null
    );
  }

  async initialize(
    input: InitializeSignalRoomInput,
  ): Promise<InitializeSignalRoomResult> {
    if (
      input.version !== SIGNAL_PROTOCOL_VERSION ||
      !Number.isSafeInteger(input.createdAt) ||
      !Number.isSafeInteger(input.expiresAt) ||
      input.expiresAt <= input.createdAt ||
      !/^[0-9a-f]{64}$/.test(input.listenerSecretHash)
    ) {
      throw new Error("invalid_signal_room_initialization");
    }

    const existing = this.row();
    if (existing !== null) {
      return {
        created: false,
        matchesInput:
          existing.listener_secret_hash === input.listenerSecretHash,
        lifecycle: existing.lifecycle,
        expiresAt: existing.expires_at,
      };
    }

    this.ctx.storage.sql.exec(
      `INSERT INTO signal_room (
        id, version, lifecycle, created_at, expires_at, listener_secret_hash
      ) VALUES (1, ?, 'listener-created', ?, ?, ?)`,
      input.version,
      input.createdAt,
      input.expiresAt,
      input.listenerSecretHash,
    );
    await this.ctx.storage.setAlarm(input.expiresAt);

    return {
      created: true,
      matchesInput: true,
      lifecycle: "listener-created",
      expiresAt: input.expiresAt,
    };
  }

  async snapshot(): Promise<{
    initialized: boolean;
    lifecycle: SignalRoomLifecycle | null;
    expiresAt: number | null;
    listenerSecretHash: "[redacted]" | null;
    terminalReason: string | null;
  }> {
    const row = this.row();
    if (row === null) {
      return {
        initialized: false,
        lifecycle: null,
        expiresAt: null,
        listenerSecretHash: null,
        terminalReason: null,
      };
    }
    return {
      initialized: true,
      lifecycle: row.lifecycle,
      expiresAt: row.expires_at,
      listenerSecretHash: "[redacted]",
      terminalReason: row.terminal_reason,
    };
  }

  override async fetch(request: Request): Promise<Response> {
    const row = this.row();
    if (row === null || row.lifecycle === "closed") {
      return signalResponse(404, "room_not_found");
    }
    if (row.expires_at <= Date.now()) {
      await this.closeRoom("expired", undefined, "room_expired", 1001);
      return signalResponse(404, "room_expired");
    }
    if (
      request.method !== "GET" ||
      request.headers.get("Upgrade")?.toLowerCase() !== "websocket"
    ) {
      return signalResponse(426, "invalid_message");
    }
    if (this.ctx.getWebSockets().length >= 4) {
      return signalResponse(429, "room_full");
    }

    const pair = new WebSocketPair();
    const client = pair[0];
    const server = pair[1];
    this.ctx.acceptWebSocket(server, ["signal-v1"]);
    server.serializeAttachment({
      version: SIGNAL_PROTOCOL_VERSION,
      sessionId: crypto.randomUUID(),
      connectedAt: Date.now(),
      bound: false,
      role: null,
    } satisfies SignalSocketAttachment);
    await this.scheduleNextAlarm();
    return new Response(null, { status: 101, webSocket: client });
  }

  override async webSocketMessage(
    ws: WebSocket,
    frame: string | ArrayBuffer,
  ): Promise<void> {
    try {
      const row = this.row();
      if (row === null || row.lifecycle === "closed") {
        this.sendErrorAndClose(ws, "room_not_found", "room is unavailable");
        return;
      }
      if (row.expires_at <= Date.now()) {
        await this.closeRoom("expired", undefined, "room_expired", 1001);
        return;
      }
      const attachment = this.attachment(ws);
      if (attachment === null) {
        this.sendErrorAndClose(
          ws,
          "internal_error",
          "socket state unavailable",
          SIGNAL_INTERNAL_CLOSE_CODE,
        );
        return;
      }

      const message = parseSignalFrame(frame);
      if (!attachment.bound) {
        await this.bindSocket(ws, attachment, message, row);
        return;
      }
      if (message.type === "bind") {
        this.sendErrorAndClose(
          ws,
          "unexpected_message",
          "socket is already bound",
        );
        return;
      }
      if (message.type === "leave") {
        await this.closeRoom("peer_left", ws, undefined, 1000);
        return;
      }
      await this.forward(ws, attachment, message, row);
    } catch (error) {
      if (error instanceof SignalProtocolError) {
        this.sendErrorAndClose(ws, error.code, "invalid signaling message");
        return;
      }
      console.error(
        JSON.stringify({
          event: "signal_protocol_error",
          code: "internal_error",
        }),
      );
      this.sendErrorAndClose(
        ws,
        "internal_error",
        "internal signaling error",
        SIGNAL_INTERNAL_CLOSE_CODE,
      );
    }
  }

  override async webSocketClose(
    ws: WebSocket,
    code: number,
    reason: string,
  ): Promise<void> {
    const attachment = this.attachment(ws);
    if (attachment?.bound) {
      await this.closeRoom("peer_left", ws, undefined, 1000);
    } else {
      await this.scheduleNextAlarm();
    }
    this.safeClose(ws, code, reason);
  }

  override async webSocketError(ws: WebSocket, _error: unknown): Promise<void> {
    console.error(
      JSON.stringify({ event: "signal_socket_error", code: "internal_error" }),
    );
    const attachment = this.attachment(ws);
    if (attachment?.bound) {
      await this.closeRoom(
        "socket_error",
        ws,
        undefined,
        SIGNAL_INTERNAL_CLOSE_CODE,
      );
    }
    this.safeClose(ws, SIGNAL_INTERNAL_CLOSE_CODE, "internal error");
  }

  override async alarm(): Promise<void> {
    const row = this.row();
    if (row === null || row.lifecycle === "closed") return;
    const now = Date.now();
    if (row.expires_at <= now) {
      await this.closeRoom(
        "expired",
        undefined,
        "room_expired",
        SIGNAL_EXPIRED_CLOSE_CODE,
      );
      return;
    }
    for (const ws of this.ctx.getWebSockets()) {
      const attachment = this.attachment(ws);
      if (
        attachment !== null &&
        !attachment.bound &&
        attachment.connectedAt + SIGNAL_BIND_DEADLINE_MS <= now
      ) {
        this.sendErrorAndClose(
          ws,
          "unexpected_message",
          "bind deadline exceeded",
        );
      }
    }
    await this.scheduleNextAlarm();
  }

  private attachment(ws: WebSocket): SignalSocketAttachment | null {
    const value = ws.deserializeAttachment();
    if (typeof value !== "object" || value === null) return null;
    const candidate = value as Partial<SignalSocketAttachment>;
    if (
      candidate.version !== SIGNAL_PROTOCOL_VERSION ||
      typeof candidate.sessionId !== "string" ||
      typeof candidate.connectedAt !== "number" ||
      typeof candidate.bound !== "boolean" ||
      !(
        candidate.role === null ||
        candidate.role === "listener" ||
        candidate.role === "joiner"
      )
    ) {
      return null;
    }
    return candidate as SignalSocketAttachment;
  }

  private boundSocket(
    role: SignalRole,
    excluding?: WebSocket,
  ): WebSocket | null {
    for (const socket of this.ctx.getWebSockets()) {
      if (socket === excluding || socket.readyState !== WebSocket.OPEN)
        continue;
      const attachment = this.attachment(socket);
      if (attachment?.bound && attachment.role === role) return socket;
    }
    return null;
  }

  private async bindSocket(
    ws: WebSocket,
    attachment: SignalSocketAttachment,
    message: ClientSignalMessage,
    row: SignalRoomRow,
  ): Promise<void> {
    if (
      Date.now() > attachment.connectedAt + SIGNAL_BIND_DEADLINE_MS ||
      message.type !== "bind"
    ) {
      this.sendErrorAndClose(
        ws,
        "unexpected_message",
        "first message must bind the socket",
      );
      return;
    }

    if (
      message.role === "listener" &&
      !(await verifyListenerSecret(message.secret, row.listener_secret_hash))
    ) {
      this.sendErrorAndClose(
        ws,
        "unauthorized_listener",
        "listener authentication failed",
      );
      return;
    }
    if (this.boundSocket(message.role, ws) !== null) {
      this.sendErrorAndClose(ws, "room_full", "role is already occupied");
      return;
    }

    ws.serializeAttachment({
      ...attachment,
      bound: true,
      role: message.role,
    } satisfies SignalSocketAttachment);
    const listener =
      message.role === "listener" ? ws : this.boundSocket("listener");
    const joiner = message.role === "joiner" ? ws : this.boundSocket("joiner");
    const lifecycle: SignalRoomLifecycle =
      listener && joiner
        ? "paired"
        : listener
          ? "listener-bound"
          : "listener-created";
    this.ctx.storage.sql.exec(
      "UPDATE signal_room SET lifecycle = ? WHERE id = 1 AND lifecycle != 'closed'",
      lifecycle,
    );

    this.safeSend(ws, {
      v: SIGNAL_PROTOCOL_VERSION,
      type: "bound",
      role: message.role,
      expires_in_seconds: Math.max(
        0,
        Math.ceil((row.expires_at - Date.now()) / 1_000),
      ),
    });
    if (listener && joiner) {
      const ready = { v: SIGNAL_PROTOCOL_VERSION, type: "peer_ready" } as const;
      this.safeSend(listener, ready);
      this.safeSend(joiner, ready);
      console.log(JSON.stringify({ event: "signal_room_paired", count: 1 }));
    }
    await this.scheduleNextAlarm();
  }

  private async forward(
    sender: WebSocket,
    attachment: SignalSocketAttachment,
    message: Exclude<ClientSignalMessage, { type: "bind" | "leave" }>,
    row: SignalRoomRow,
  ): Promise<void> {
    const role = attachment.role;
    if (role === null) {
      this.sendErrorAndClose(
        sender,
        "unexpected_message",
        "socket is not bound",
      );
      return;
    }
    const peer = this.boundSocket(role === "listener" ? "joiner" : "listener");
    if (peer === null) {
      this.sendErrorAndClose(sender, "unexpected_message", "peer is not ready");
      return;
    }

    if (message.type === "description") {
      const legalOffer =
        role === "joiner" &&
        message.sdp_type === "offer" &&
        row.offer_seen === 0;
      const legalAnswer =
        role === "listener" &&
        message.sdp_type === "answer" &&
        row.offer_seen === 1 &&
        row.answer_seen === 0;
      if (!legalOffer && !legalAnswer) {
        this.sendErrorAndClose(
          sender,
          "unexpected_message",
          "description is not valid in this state",
        );
        return;
      }
    }

    const encoded = serializeSignalMessage(message as ServerSignalMessage);
    const bytes = utf8.encode(encoded).byteLength;
    if (row.forwarded_bytes + bytes > MAX_SIGNAL_FORWARDED_BYTES) {
      this.sendErrorAndClose(
        sender,
        "resource_limit",
        "forwarded byte limit exceeded",
      );
      return;
    }

    if (message.type === "candidate") {
      const countColumn =
        role === "listener" ? "listener_candidates" : "joiner_candidates";
      const count =
        role === "listener" ? row.listener_candidates : row.joiner_candidates;
      if (count >= MAX_SIGNAL_CANDIDATES_PER_PEER) {
        this.sendErrorAndClose(
          sender,
          "resource_limit",
          "candidate limit exceeded",
        );
        return;
      }
      this.ctx.storage.sql.exec(
        `UPDATE signal_room
            SET ${countColumn} = ${countColumn} + 1,
                forwarded_bytes = forwarded_bytes + ?
          WHERE id = 1 AND lifecycle != 'closed'`,
        bytes,
      );
    } else if (message.type === "description") {
      const flag = message.sdp_type === "offer" ? "offer_seen" : "answer_seen";
      this.ctx.storage.sql.exec(
        `UPDATE signal_room
            SET ${flag} = 1, forwarded_bytes = forwarded_bytes + ?
          WHERE id = 1 AND lifecycle != 'closed'`,
        bytes,
      );
    } else {
      this.ctx.storage.sql.exec(
        `UPDATE signal_room
            SET forwarded_bytes = forwarded_bytes + ?
          WHERE id = 1 AND lifecycle != 'closed'`,
        bytes,
      );
    }
    this.safeSendSerialized(peer, encoded);
  }

  private safeSend(ws: WebSocket, message: ServerSignalMessage): void {
    this.safeSendSerialized(ws, serializeSignalMessage(message));
  }

  private safeSendSerialized(ws: WebSocket, message: string): void {
    if (ws.readyState !== WebSocket.OPEN) return;
    try {
      ws.send(message);
    } catch {
      // The close/error callback owns terminal cleanup. Do not log payloads.
    }
  }

  private sendErrorAndClose(
    ws: WebSocket,
    code: SignalErrorCode,
    message: string,
    closeCode = SIGNAL_POLICY_CLOSE_CODE,
  ): void {
    this.safeSend(ws, {
      v: SIGNAL_PROTOCOL_VERSION,
      type: "error",
      code,
      message,
    });
    console.log(JSON.stringify({ event: "signal_protocol_error", code }));
    this.safeClose(ws, closeCode, code);
  }

  private safeClose(ws: WebSocket, code: number, reason: string): void {
    try {
      if (
        ws.readyState === WebSocket.OPEN ||
        ws.readyState === WebSocket.CONNECTING
      ) {
        ws.close(code, reason.slice(0, 123));
      }
    } catch {
      // Idempotent cleanup: an already-closing socket needs no second action.
    }
  }

  private async closeRoom(
    terminalReason: string,
    source?: WebSocket,
    errorCode?: SignalErrorCode,
    closeCode = 1000,
  ): Promise<void> {
    const row = this.row();
    if (row === null || row.lifecycle === "closed") return;
    this.ctx.storage.sql.exec(
      `UPDATE signal_room
          SET lifecycle = 'closed', terminal_reason = ?
        WHERE id = 1 AND lifecycle != 'closed'`,
      terminalReason,
    );
    await this.ctx.storage.deleteAlarm();
    for (const socket of this.ctx.getWebSockets()) {
      if (socket !== source) {
        if (errorCode) {
          this.safeSend(socket, {
            v: SIGNAL_PROTOCOL_VERSION,
            type: "error",
            code: errorCode,
            message: "room expired",
          });
        } else {
          this.safeSend(socket, {
            v: SIGNAL_PROTOCOL_VERSION,
            type: "peer_left",
          });
        }
      }
      this.safeClose(socket, closeCode, terminalReason);
    }
  }

  private async scheduleNextAlarm(): Promise<void> {
    const row = this.row();
    if (row === null || row.lifecycle === "closed") {
      await this.ctx.storage.deleteAlarm();
      return;
    }
    let next = row.expires_at;
    for (const socket of this.ctx.getWebSockets()) {
      const attachment = this.attachment(socket);
      if (attachment !== null && !attachment.bound) {
        next = Math.min(next, attachment.connectedAt + SIGNAL_BIND_DEADLINE_MS);
      }
    }
    await this.ctx.storage.setAlarm(next);
  }
}
