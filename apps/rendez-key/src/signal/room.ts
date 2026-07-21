import { DurableObject } from "cloudflare:workers";
import { SIGNAL_PROTOCOL_VERSION } from "./limits";

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

  override async alarm(): Promise<void> {
    const row = this.row();
    if (row === null || row.lifecycle === "closed") return;
    if (row.expires_at > Date.now()) {
      await this.ctx.storage.setAlarm(row.expires_at);
      return;
    }
    this.ctx.storage.sql.exec(
      `UPDATE signal_room
          SET lifecycle = 'closed', terminal_reason = 'expired'
        WHERE id = 1 AND lifecycle != 'closed'`,
    );
  }
}
