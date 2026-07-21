(() => {
  "use strict";

  const SIGNAL_VERSION = 1;
  const SUBPROTOCOL = "netsu/iperf3-webrtc/1";
  const CONTROL_LABEL = "netsu-control";
  const MAX_SIGNAL_FRAME = 65_536;
  const MAX_SIGNAL_SDP = 60 * 1_024;
  const MAX_SIGNAL_CANDIDATE = 4_096;
  const MAX_SIGNAL_CANDIDATES = 16;
  const MAX_SIGNAL_BYTES = 1_048_576;
  const MAX_JSON = 65_536;
  const MAX_MESSAGE = 16 * 1_024;
  const SEND_HIGH_WATERMARK = 4 * 1_024 * 1_024;
  const SEND_LOW_WATERMARK = 1 * 1_024 * 1_024;
  const DRAIN_TIMEOUT_MS = 5_000;
  const SETUP_TIMEOUT_MS = 20_000;

  const STATE = Object.freeze({
    TEST_START: 1,
    TEST_RUNNING: 2,
    TEST_END: 4,
    PARAM_EXCHANGE: 9,
    CREATE_STREAMS: 10,
    EXCHANGE_RESULTS: 13,
    DISPLAY_RESULTS: 14,
    IPERF_START: 15,
    IPERF_DONE: 16,
    ACCESS_DENIED: -1,
    SERVER_ERROR: -2,
  });

  class NetsuBrowserError extends Error {
    constructor(kind, message) {
      super(message);
      this.name = "NetsuBrowserError";
      this.kind = kind;
    }
  }

  function fail(kind, message) {
    throw new NetsuBrowserError(kind, message);
  }

  function delay(milliseconds) {
    return new Promise((resolve) => setTimeout(resolve, milliseconds));
  }

  async function withTimeout(promise, milliseconds, message) {
    let timer;
    try {
      return await Promise.race([
        promise,
        new Promise((_, reject) => {
          timer = setTimeout(
            () => reject(new NetsuBrowserError("setup_timeout", message)),
            milliseconds,
          );
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }

  function byteLength(text) {
    return new TextEncoder().encode(text).byteLength;
  }

  function normalizeRoomCode(input) {
    const compact = String(input).toUpperCase().replaceAll("-", "");
    if (!/^[23456789ABCDEFGHJKLMNPQRSTUVWXYZ]{8}$/.test(compact)) {
      fail("config", "invalid signaling room code");
    }
    return `${compact.slice(0, 4)}-${compact.slice(4)}`;
  }

  function roomWebSocketUrl(signalUrl, code) {
    const url = new URL(signalUrl);
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      fail("config", "signal URL must use HTTP or HTTPS");
    }
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    url.pathname = `${url.pathname.replace(/\/+$/, "")}/rooms/${normalizeRoomCode(code)}/ws`;
    url.search = "";
    url.hash = "";
    return url.toString();
  }

  function validateStunUrls(values) {
    if (!Array.isArray(values) || values.length > 4) {
      fail("config", "at most 4 STUN URLs are allowed");
    }
    for (const value of values) {
      if (typeof value !== "string" || !value.toLowerCase().startsWith("stun:")) {
        fail("config", "only STUN URLs are allowed; TURN relay is unsupported");
      }
    }
    return values;
  }

  function validateConfig(config) {
    const parallel = Number(config.parallel ?? 1);
    const duration = Number(config.duration ?? 1);
    const length = Number(config.length ?? 65_536);
    if (!Number.isInteger(parallel) || parallel < 1 || parallel > 128) {
      fail("config", "parallel must be an integer in 1..128");
    }
    if (!Number.isFinite(duration) || duration < 1 || duration > 86_400) {
      fail("config", "duration must be in 1..86400 seconds");
    }
    if (!Number.isInteger(length) || length < 4 || length > 1_048_576) {
      fail("config", "length must be an integer in 4..1048576 bytes");
    }
    let signalUrl;
    try {
      signalUrl = new URL(config.signalUrl).toString();
    } catch {
      fail("config", "signal URL must be a valid HTTP or HTTPS URL");
    }
    if (!signalUrl.startsWith("http://") && !signalUrl.startsWith("https://")) {
      fail("config", "signal URL must use HTTP or HTTPS");
    }
    return {
      code: normalizeRoomCode(config.code),
      signalUrl,
      stunUrls: validateStunUrls(config.stunUrls ?? []),
      includeAddresses: config.includeAddresses === true,
      reverse: config.reverse === true,
      parallel,
      duration,
      length,
    };
  }

  function validateSignal(message) {
    if (!message || message.v !== SIGNAL_VERSION || typeof message.type !== "string") {
      fail("signaling", "invalid signaling protocol message");
    }
    if (message.type === "description") {
      if (
        !["offer", "answer"].includes(message.sdp_type) ||
        typeof message.sdp !== "string" ||
        byteLength(message.sdp) === 0 ||
        byteLength(message.sdp) > MAX_SIGNAL_SDP
      ) {
        fail("signaling", "invalid signaling description");
      }
    }
    if (message.type === "candidate") {
      if (
        typeof message.candidate !== "string" ||
        byteLength(message.candidate) === 0 ||
        byteLength(message.candidate) > MAX_SIGNAL_CANDIDATE
      ) {
        fail("signaling", "invalid signaling candidate");
      }
    }
    return message;
  }

  function makeBind(role, secret) {
    if (role === "listener" && (typeof secret !== "string" || secret.length === 0)) {
      fail("signaling", "listener bind requires a non-empty secret");
    }
    if (role === "joiner" && secret != null) {
      fail("signaling", "joiner bind must not include a listener secret");
    }
    return secret == null
      ? { v: SIGNAL_VERSION, type: "bind", role }
      : { v: SIGNAL_VERSION, type: "bind", role, secret };
  }

  class AsyncQueue {
    constructor() {
      this.values = [];
      this.waiters = [];
      this.error = null;
      this.closed = false;
    }

    push(value) {
      if (this.closed) return;
      const waiter = this.waiters.shift();
      if (waiter) waiter.resolve(value);
      else this.values.push(value);
    }

    fail(error) {
      if (this.closed) return;
      this.error = error;
      this.closed = true;
      for (const waiter of this.waiters.splice(0)) waiter.reject(error);
    }

    close() {
      if (this.closed) return;
      this.closed = true;
      const error = new NetsuBrowserError("transport_closed", "queue closed");
      for (const waiter of this.waiters.splice(0)) waiter.reject(error);
    }

    async next(timeoutMs = SETUP_TIMEOUT_MS, label = "operation timed out") {
      if (this.values.length > 0) return this.values.shift();
      if (this.error) throw this.error;
      if (this.closed) fail("transport_closed", "queue closed");
      return withTimeout(
        new Promise((resolve, reject) => this.waiters.push({ resolve, reject })),
        timeoutMs,
        label,
      );
    }
  }

  class ByteQueue {
    constructor(limit = MAX_JSON + 8) {
      this.parts = [];
      this.length = 0;
      this.limit = limit;
      this.changed = new AsyncQueue();
      this.error = null;
      this.closed = false;
    }

    feed(value) {
      const bytes = value instanceof Uint8Array ? value : new Uint8Array(value);
      if (this.closed) return;
      if (this.length + bytes.byteLength > this.limit) {
        this.fail(new NetsuBrowserError("protocol_error", "control receive queue exceeded limit"));
        return;
      }
      if (bytes.byteLength > 0) {
        this.parts.push(bytes.slice());
        this.length += bytes.byteLength;
      }
      this.changed.push(true);
    }

    fail(error) {
      this.error = error;
      this.closed = true;
      this.changed.fail(error);
    }

    close() {
      this.closed = true;
      this.changed.close();
    }

    async readExact(count, timeoutMs = 30_000) {
      const deadline = performance.now() + timeoutMs;
      while (this.length < count) {
        if (this.error) throw this.error;
        if (this.closed) fail("transport_closed", "control DataChannel closed");
        const remaining = deadline - performance.now();
        if (remaining <= 0) fail("setup_timeout", "control read timed out");
        await this.changed.next(remaining, "control read timed out");
      }
      const output = new Uint8Array(count);
      let offset = 0;
      while (offset < count) {
        const part = this.parts[0];
        const take = Math.min(part.byteLength, count - offset);
        output.set(part.subarray(0, take), offset);
        offset += take;
        this.length -= take;
        if (take === part.byteLength) this.parts.shift();
        else this.parts[0] = part.subarray(take);
      }
      return output;
    }
  }

  class SignalSocket {
    constructor(url) {
      this.url = url;
      this.socket = null;
      this.incoming = new AsyncQueue();
      this.sentCandidates = 0;
      this.sentBytes = 0;
    }

    async open() {
      const socket = new WebSocket(this.url);
      this.socket = socket;
      socket.addEventListener("message", (event) => {
        try {
          if (typeof event.data !== "string" || byteLength(event.data) > MAX_SIGNAL_FRAME) {
            fail("signaling", "signaling server sent an invalid frame");
          }
          this.incoming.push(validateSignal(JSON.parse(event.data)));
        } catch (error) {
          this.incoming.fail(error);
        }
      });
      // Browsers deliberately hide WebSocket transport details on `error`.
      // The following `close` event carries the sanitized application code,
      // so use it as the established-session failure signal instead.
      socket.addEventListener("error", () => {});
      socket.addEventListener("close", (event) =>
        this.incoming.fail(
          new NetsuBrowserError(
            "signaling",
            `signaling WebSocket closed (${event.code || 1006})`,
          ),
        ),
      );
      await withTimeout(
        new Promise((resolve, reject) => {
          socket.addEventListener("open", resolve, { once: true });
          socket.addEventListener(
            "error",
            () => reject(new NetsuBrowserError("signaling", "signaling WebSocket failed")),
            { once: true },
          );
        }),
        10_000,
        "signaling connection timed out",
      );
    }

    send(message) {
      validateSignal(message);
      if (message.type === "candidate" && ++this.sentCandidates > MAX_SIGNAL_CANDIDATES) {
        fail("signaling", "signaling candidate limit exceeded");
      }
      const frame = JSON.stringify(message);
      const length = byteLength(frame);
      if (length > MAX_SIGNAL_FRAME) fail("signaling", "signaling frame exceeds 64 KiB");
      this.sentBytes += length;
      if (this.sentBytes > MAX_SIGNAL_BYTES) {
        fail("signaling", "signaling forwarded byte limit exceeded");
      }
      if (!this.socket || this.socket.readyState !== WebSocket.OPEN) {
        fail("transport_closed", "signaling WebSocket is closed");
      }
      this.socket.send(frame);
    }

    next(timeoutMs = 15_000) {
      return this.incoming.next(timeoutMs, "signaling exchange timed out");
    }

    close() {
      if (this.socket?.readyState === WebSocket.OPEN) {
        this.send({ v: SIGNAL_VERSION, type: "leave" });
        this.socket.close(1000);
      }
    }
  }

  function signedState(byte) {
    return byte > 127 ? byte - 256 : byte;
  }

  function stateBytes(state) {
    return Uint8Array.of(state & 0xff);
  }

  function encodeJson(value) {
    const body = new TextEncoder().encode(JSON.stringify(value));
    if (body.byteLength > MAX_JSON) fail("protocol_error", "JSON exceeds 64 KiB");
    const framed = new Uint8Array(4 + body.byteLength);
    new DataView(framed.buffer).setUint32(0, body.byteLength, false);
    framed.set(body, 4);
    return framed;
  }

  async function readJson(queue) {
    const prefix = await queue.readExact(4);
    const length = new DataView(prefix.buffer, prefix.byteOffset, 4).getUint32(0, false);
    if (length > MAX_JSON) fail("protocol_error", "JSON exceeds 64 KiB");
    const body = await queue.readExact(length);
    try {
      return JSON.parse(new TextDecoder().decode(body));
    } catch {
      fail("protocol_error", "invalid control JSON");
    }
  }

  function nextStreamId(existingCount) {
    return existingCount === 0 ? 1 : existingCount + 2;
  }

  function buildParams(config) {
    const params = {
      omit: 0,
      time: config.duration,
      num: 0,
      blockcount: 0,
      parallel: config.parallel,
      len: config.length,
      pacing_timer: 1000,
      client_version: "netsu-browser-0.1.0",
      tcp: true,
    };
    if (config.reverse) params.reverse = true;
    return params;
  }

  function makeCookie() {
    const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
    const random = new Uint8Array(36);
    crypto.getRandomValues(random);
    const cookie = new Uint8Array(37);
    for (let index = 0; index < random.length; index += 1) {
      cookie[index] = alphabet.charCodeAt(random[index] % alphabet.length);
    }
    return cookie;
  }

  function randomBytes(length) {
    const output = new Uint8Array(length);
    for (let offset = 0; offset < output.length; offset += 65_536) {
      crypto.getRandomValues(output.subarray(offset, Math.min(output.length, offset + 65_536)));
    }
    return output;
  }

  function validateDataChannel(channel, expectedLabel) {
    if (
      channel.label !== expectedLabel ||
      channel.protocol !== SUBPROTOCOL ||
      channel.ordered !== true ||
      channel.maxPacketLifeTime != null ||
      channel.maxRetransmits != null ||
      channel.negotiated === true
    ) {
      fail("protocol_error", `invalid WebRTC DataChannel ${expectedLabel}`);
    }
  }

  async function waitForChannelOpen(channel, timeoutMs = 10_000) {
    if (channel.readyState === "open") return;
    await withTimeout(
      new Promise((resolve, reject) => {
        channel.addEventListener("open", resolve, { once: true });
        channel.addEventListener(
          "close",
          () => reject(new NetsuBrowserError("transport_closed", "DataChannel closed")),
          { once: true },
        );
      }),
      timeoutMs,
      "DataChannel open timed out",
    );
  }

  async function waitBufferedAtMost(channel, maximum, timeoutMs) {
    if (channel.bufferedAmount <= maximum) return;
    channel.bufferedAmountLowThreshold = maximum;
    await withTimeout(
      new Promise((resolve, reject) => {
        const cleanup = () => {
          channel.removeEventListener("bufferedamountlow", onLow);
          channel.removeEventListener("close", onClosed);
          channel.removeEventListener("error", onClosed);
        };
        const onLow = () => {
          if (channel.bufferedAmount <= maximum) {
            cleanup();
            resolve();
          }
        };
        const onClosed = () => {
          cleanup();
          reject(new NetsuBrowserError("transport_closed", "DataChannel closed"));
        };
        channel.addEventListener("bufferedamountlow", onLow);
        channel.addEventListener("close", onClosed);
        channel.addEventListener("error", onClosed);
        onLow();
      }),
      timeoutMs,
      "DataChannel drain timed out",
    );
  }

  async function sendBytes(channel, bytes) {
    for (let offset = 0; offset < bytes.byteLength; offset += MAX_MESSAGE) {
      if (channel.bufferedAmount >= SEND_HIGH_WATERMARK) {
        await waitBufferedAtMost(channel, SEND_LOW_WATERMARK, DRAIN_TIMEOUT_MS);
      }
      channel.send(bytes.slice(offset, Math.min(bytes.byteLength, offset + MAX_MESSAGE)));
    }
  }

  async function drainAndFinish(channel) {
    await waitBufferedAtMost(channel, 0, DRAIN_TIMEOUT_MS);
    channel.send(new Uint8Array(0));
  }

  function streamResults(counters, durationSeconds, sender) {
    return {
      sender_has_retransmits: sender ? 0 : -1,
      streams: counters.map((bytes, index) => ({
        id: nextStreamId(index),
        bytes,
        retransmits: -1,
        jitter: 0,
        errors: 0,
        omitted_errors: 0,
        packets: 0,
        omitted_packets: 0,
        start_time: 0,
        end_time: durationSeconds,
      })),
    };
  }

  function candidateAddress(candidate) {
    const address = candidate.address ?? candidate.ip;
    return address == null ? null : `${address}:${candidate.port ?? 0}`;
  }

  function directPairFromStats(stats, includeAddresses) {
    let pair;
    for (const report of stats.values()) {
      if (report.type === "transport" && report.selectedCandidatePairId) {
        pair = stats.get(report.selectedCandidatePairId);
        break;
      }
    }
    if (!pair) {
      pair = [...stats.values()].find(
        (report) =>
          report.type === "candidate-pair" &&
          report.state === "succeeded" &&
          (report.nominated === true || report.selected === true),
      );
    }
    if (!pair) fail("direct_path_unavailable", "direct path is unavailable");
    const local = stats.get(pair.localCandidateId);
    const remote = stats.get(pair.remoteCandidateId);
    const allowed = new Set(["host", "srflx", "prflx"]);
    const protocol = local?.protocol ?? remote?.protocol;
    if (
      !local ||
      !remote ||
      !allowed.has(local.candidateType) ||
      !allowed.has(remote.candidateType) ||
      !["udp", "tcp"].includes(protocol)
    ) {
      fail("direct_path_unavailable", "direct path is unavailable");
    }
    return {
      path: "direct",
      local_candidate_type: local.candidateType,
      remote_candidate_type: remote.candidateType,
      ice_protocol: protocol,
      addresses_included: includeAddresses,
      ...(includeAddresses
        ? { local_addr: candidateAddress(local), remote_addr: candidateAddress(remote) }
        : {}),
    };
  }

  async function waitForDirectPair(peer, includeAddresses) {
    const deadline = performance.now() + 5_000;
    while (performance.now() < deadline) {
      try {
        return directPairFromStats(await peer.getStats(), includeAddresses);
      } catch (error) {
        if (error.kind !== "direct_path_unavailable") throw error;
      }
      await delay(50);
    }
    fail("direct_path_unavailable", "direct path is unavailable");
  }

  function errorResult(error) {
    const kind = error instanceof NetsuBrowserError ? error.kind : "runtime_error";
    return {
      error: {
        transport: "webrtc",
        kind,
        message:
          kind === "direct_path_unavailable"
            ? "WebRTC direct connection failed; netsu does not use TURN relay, so no throughput test was run"
            : String(error?.message ?? error),
      },
    };
  }

  function validateChannelManifest(channels, parallel) {
    const labels = new Set();
    for (const channel of channels) {
      validateDataChannel(channel, channel.label);
      if (labels.has(channel.label)) fail("protocol_error", "duplicate DataChannel label");
      labels.add(channel.label);
    }
    if (!labels.has(CONTROL_LABEL)) fail("channels_missing", "control DataChannel is missing");
    for (let index = 0; index < parallel; index += 1) {
      if (!labels.has(`netsu-data/${index}`)) {
        fail("channels_missing", `payload DataChannel ${index} is missing`);
      }
    }
    if (labels.size !== parallel + 1) fail("protocol_error", "unexpected DataChannel label");
    return true;
  }

  class BrowserPeer {
    constructor(input) {
      this.config = validateConfig(input);
      this.signal = null;
      this.peer = null;
      this.control = null;
      this.controlQueue = new ByteQueue();
      this.localIce = new AsyncQueue();
      this.payloads = [];
      this.counters = Array(this.config.parallel).fill(0);
      this.eof = [];
      this.remoteResults = null;
      this.connection = null;
      this.startedAt = null;
      this.endedAt = null;
    }

    async run() {
      try {
        await this.connect();
        return await this.runProtocol();
      } finally {
        try {
          this.signal?.close();
        } catch {}
        for (const channel of this.payloads) {
          try {
            channel.close();
          } catch {}
        }
        try {
          this.control?.close();
        } catch {}
        try {
          this.peer?.close();
        } catch {}
      }
    }

    async connect() {
      this.signal = new SignalSocket(
        roomWebSocketUrl(this.config.signalUrl, this.config.code),
      );
      await this.signal.open();
      this.signal.send(makeBind("joiner"));
      const bound = await this.signal.next(10_000);
      if (bound.type !== "bound" || bound.role !== "joiner") {
        fail("signaling", "joiner bind was not acknowledged");
      }
      const ready = await this.signal.next(10_000);
      if (ready.type !== "peer_ready") fail("signaling", "listener was not ready");

      this.peer = new RTCPeerConnection({
        iceServers: this.config.stunUrls.map((urls) => ({ urls })),
        iceTransportPolicy: "all",
      });
      this.peer.addEventListener("icecandidate", (event) =>
        this.localIce.push(event.candidate?.toJSON() ?? null),
      );
      this.peer.addEventListener("datachannel", (event) => {
        event.channel.close();
        this.controlQueue.fail(
          new NetsuBrowserError("protocol_error", "answerer opened an unexpected DataChannel"),
        );
      });

      this.control = this.peer.createDataChannel(CONTROL_LABEL, {
        ordered: true,
        protocol: SUBPROTOCOL,
      });
      this.attachControl();
      validateDataChannel(this.control, CONTROL_LABEL);

      const offer = await this.peer.createOffer();
      await this.peer.setLocalDescription(offer);
      this.signal.send({
        v: SIGNAL_VERSION,
        type: "description",
        sdp_type: "offer",
        sdp: this.peer.localDescription.sdp,
      });

      await Promise.all([this.sendLocalIce(), this.receiveRemoteIce()]);
      await Promise.all([
        waitForChannelOpen(this.control),
        this.waitForConnected(),
      ]);
      this.connection = await waitForDirectPair(this.peer, this.config.includeAddresses);
      this.signal.close();
    }

    attachControl() {
      this.control.binaryType = "arraybuffer";
      this.control.addEventListener("message", (event) => {
        if (typeof event.data === "string") {
          this.controlQueue.fail(
            new NetsuBrowserError("protocol_error", "control DataChannel sent text"),
          );
          return;
        }
        if (event.data instanceof ArrayBuffer) this.controlQueue.feed(event.data);
        else if (event.data instanceof Blob) {
          event.data
            .arrayBuffer()
            .then((value) => this.controlQueue.feed(value))
            .catch((error) => this.controlQueue.fail(error));
        } else {
          this.controlQueue.fail(
            new NetsuBrowserError("protocol_error", "unsupported control message"),
          );
        }
      });
      this.control.addEventListener("close", () => this.controlQueue.close());
      this.control.addEventListener("error", () =>
        this.controlQueue.fail(
          new NetsuBrowserError("transport_closed", "control DataChannel failed"),
        ),
      );
    }

    async sendLocalIce() {
      while (true) {
        const candidate = await this.localIce.next(15_000, "ICE gathering timed out");
        if (candidate == null) {
          this.signal.send({ v: SIGNAL_VERSION, type: "end_of_candidates" });
          return;
        }
        this.signal.send({
          v: SIGNAL_VERSION,
          type: "candidate",
          candidate: candidate.candidate,
          sdp_mid: candidate.sdpMid ?? null,
          sdp_mline_index: candidate.sdpMLineIndex ?? null,
          username_fragment: candidate.usernameFragment ?? null,
        });
      }
    }

    async receiveRemoteIce() {
      let answered = false;
      const pending = [];
      while (true) {
        const message = await this.signal.next();
        if (message.type === "description" && message.sdp_type === "answer" && !answered) {
          await this.peer.setRemoteDescription({ type: "answer", sdp: message.sdp });
          answered = true;
          for (const candidate of pending.splice(0)) await this.peer.addIceCandidate(candidate);
        } else if (message.type === "candidate") {
          const candidate = {
            candidate: message.candidate,
            sdpMid: message.sdp_mid ?? null,
            sdpMLineIndex: message.sdp_mline_index ?? null,
            usernameFragment: message.username_fragment ?? null,
          };
          if (answered) await this.peer.addIceCandidate(candidate);
          else pending.push(candidate);
        } else if (message.type === "end_of_candidates" && answered) {
          await this.peer.addIceCandidate(null);
          return;
        } else if (message.type === "error" || message.type === "peer_left") {
          fail("transport_closed", "signaling peer left during negotiation");
        } else {
          fail("signaling", "unexpected signaling message");
        }
      }
    }

    async waitForConnected() {
      if (this.peer.connectionState === "connected") return;
      await withTimeout(
        new Promise((resolve, reject) => {
          const changed = () => {
            if (this.peer.connectionState === "connected") resolve();
            if (["failed", "closed"].includes(this.peer.connectionState)) {
              reject(
                new NetsuBrowserError("direct_path_unavailable", "direct path is unavailable"),
              );
            }
          };
          this.peer.addEventListener("connectionstatechange", changed);
          changed();
        }),
        SETUP_TIMEOUT_MS,
        "WebRTC direct connection timed out",
      ).catch((error) => {
        if (error.kind === "setup_timeout") {
          fail("direct_path_unavailable", "direct path is unavailable");
        }
        throw error;
      });
    }

    async writeControl(bytes) {
      await sendBytes(this.control, bytes);
    }

    async readState() {
      return signedState((await this.controlQueue.readExact(1))[0]);
    }

    async writeState(state) {
      await this.writeControl(stateBytes(state));
    }

    async openPayloads() {
      const opens = [];
      for (let index = 0; index < this.config.parallel; index += 1) {
        const label = `netsu-data/${index}`;
        const channel = this.peer.createDataChannel(label, {
          ordered: true,
          protocol: SUBPROTOCOL,
        });
        channel.binaryType = "arraybuffer";
        validateDataChannel(channel, label);
        this.payloads.push(channel);
        if (this.config.reverse) this.attachPayloadReceiver(channel, index);
        opens.push(waitForChannelOpen(channel));
      }
      await Promise.all(opens);
    }

    attachPayloadReceiver(channel, index) {
      let resolveEof;
      const eof = new Promise((resolve) => {
        resolveEof = resolve;
      });
      this.eof[index] = eof;
      channel.addEventListener("message", (event) => {
        if (typeof event.data === "string") {
          this.controlQueue.fail(
            new NetsuBrowserError("protocol_error", "payload DataChannel sent text"),
          );
          return;
        }
        const count = event.data instanceof ArrayBuffer ? event.data.byteLength : event.data.size;
        if (count === 0) resolveEof();
        else this.counters[index] += count;
      });
    }

    async transfer() {
      this.startedAt = performance.now();
      const deadline = this.startedAt + this.config.duration * 1_000;
      if (this.config.reverse) {
        await delay(Math.max(0, deadline - performance.now()));
        this.endedAt = performance.now();
        await this.writeState(STATE.TEST_END);
        return;
      }

      const chunk = randomBytes(this.config.length);
      await Promise.all(
        this.payloads.map(async (channel, index) => {
          let writes = 0;
          while (performance.now() < deadline) {
            await sendBytes(channel, chunk);
            this.counters[index] += chunk.byteLength;
            writes += 1;
            if (writes % 64 === 0) await delay(0);
          }
        }),
      );
      this.endedAt = performance.now();
      await Promise.all(this.payloads.map((channel) => drainAndFinish(channel)));
      await this.writeState(STATE.TEST_END);
    }

    async runProtocol() {
      await this.writeControl(makeCookie());
      while (true) {
        const state = await this.readState();
        if (state === STATE.PARAM_EXCHANGE) {
          await this.writeControl(encodeJson(buildParams(this.config)));
        } else if (state === STATE.CREATE_STREAMS) {
          await this.openPayloads();
        } else if (state === STATE.TEST_START || state === STATE.IPERF_START) {
          continue;
        } else if (state === STATE.TEST_RUNNING) {
          await this.transfer();
        } else if (state === STATE.EXCHANGE_RESULTS) {
          if (this.config.reverse) {
            await withTimeout(
              Promise.all(this.eof),
              DRAIN_TIMEOUT_MS + 1_000,
              "payload end markers timed out",
            );
          }
          const seconds = Math.max(0, this.endedAt - this.startedAt) / 1_000;
          await this.writeControl(
            encodeJson(streamResults(this.counters, seconds, !this.config.reverse)),
          );
          this.remoteResults = await readJson(this.controlQueue);
        } else if (state === STATE.DISPLAY_RESULTS) {
          await this.writeState(STATE.IPERF_DONE);
          const acknowledged = await this.readState();
          if (acknowledged !== STATE.IPERF_DONE) {
            fail("protocol_error", "terminal acknowledgement mismatch");
          }
          return this.result();
        } else if (state === STATE.ACCESS_DENIED) {
          fail("protocol_error", "server denied the WebRTC test");
        } else if (state === STATE.SERVER_ERROR) {
          fail("protocol_error", "server reported a WebRTC test error");
        } else {
          fail("protocol_error", `unexpected control state ${state}`);
        }
      }
    }

    result() {
      const durationSeconds = Math.max(0, this.endedAt - this.startedAt) / 1_000;
      const local = streamResults(this.counters, durationSeconds, !this.config.reverse);
      const localBytes = this.counters.reduce((sum, value) => sum + value, 0);
      const remoteBytes = this.remoteResults.streams.reduce(
        (sum, stream) => sum + Number(stream.bytes),
        0,
      );
      const drift = Math.abs(localBytes - remoteBytes) / Math.max(localBytes, remoteBytes, 1);
      if (drift > 0.05) {
        fail("byte_drift", "WebRTC application byte drift exceeded 5%");
      }
      const sentBytes = this.config.reverse ? remoteBytes : localBytes;
      const receivedBytes = this.config.reverse ? localBytes : remoteBytes;
      return {
        transport: "webrtc",
        reverse: this.config.reverse,
        parallel: this.config.parallel,
        duration_seconds: durationSeconds,
        sent_bytes: sentBytes,
        received_bytes: receivedBytes,
        send_bits_per_second: (sentBytes * 8) / Math.max(durationSeconds, Number.EPSILON),
        receive_bits_per_second: (receivedBytes * 8) / Math.max(durationSeconds, Number.EPSILON),
        local,
        remote: this.remoteResults,
        connection: this.connection,
      };
    }
  }

  async function run(config) {
    return new BrowserPeer(config).run();
  }

  globalThis.NetsuBrowserPeer = Object.freeze({
    run,
    errorResult,
    internals: Object.freeze({
      AsyncQueue,
      ByteQueue,
      NetsuBrowserError,
      STATE,
      SUBPROTOCOL,
      CONTROL_LABEL,
      buildParams,
      directPairFromStats,
      drainAndFinish,
      encodeJson,
      makeBind,
      makeCookie,
      nextStreamId,
      normalizeRoomCode,
      readJson,
      roomWebSocketUrl,
      sendBytes,
      signedState,
      stateBytes,
      streamResults,
      validateConfig,
      validateDataChannel,
      validateChannelManifest,
      validateManifest: validateChannelManifest,
      validateSignal,
      waitBufferedAtMost,
      waitForChannelOpen,
    }),
  });
})();
