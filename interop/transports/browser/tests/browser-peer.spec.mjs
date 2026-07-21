import { expect, test } from "@playwright/test";
import { fileURLToPath } from "node:url";

const peerScript = fileURLToPath(new URL("../browser-peer.js", import.meta.url));

async function load(page) {
  await page.goto("about:blank");
  await page.addScriptTag({ path: peerScript });
  await page.waitForFunction(() => Boolean(globalThis.NetsuBrowserPeer));
}

test.beforeEach(async ({ page }) => load(page));

test("framing, cookie, and stream IDs match the independent wire contract", async ({ page }) => {
  const result = await page.evaluate(() => {
    const api = globalThis.NetsuBrowserPeer.internals;
    const frame = api.encodeJson({ tcp: true, parallel: 4 });
    const cookie = api.makeCookie();
    return {
      declaredLength: new DataView(frame.buffer).getUint32(0, false),
      body: new TextDecoder().decode(frame.slice(4)),
      ids: [0, 1, 2, 3].map(api.nextStreamId),
      cookieLength: cookie.byteLength,
      cookieTerminator: cookie[36],
    };
  });
  expect(result.declaredLength).toBe(new TextEncoder().encode(result.body).byteLength);
  expect(JSON.parse(result.body)).toEqual({ tcp: true, parallel: 4 });
  expect(result.ids).toEqual([1, 3, 4, 5]);
  expect(result.cookieLength).toBe(37);
  expect(result.cookieTerminator).toBe(0);
});

test("control JSON reassembles across arbitrary message boundaries", async ({ page }) => {
  const result = await page.evaluate(async () => {
    const api = globalThis.NetsuBrowserPeer.internals;
    const queue = new api.ByteQueue();
    const frame = api.encodeJson({ nested: { answer: 42 }, text: "netsu" });
    queue.feed(frame.slice(0, 2));
    queue.feed(frame.slice(2, 7));
    queue.feed(frame.slice(7));
    return api.readJson(queue);
  });
  expect(result).toEqual({ nested: { answer: 42 }, text: "netsu" });
});

test("a real Chromium DataChannel fragments payload and observes the EOF marker", async ({
  page,
}) => {
  const result = await page.evaluate(async () => {
    const api = globalThis.NetsuBrowserPeer.internals;
    const left = new RTCPeerConnection();
    const right = new RTCPeerConnection();
    left.addEventListener("icecandidate", (event) => {
      if (event.candidate) void right.addIceCandidate(event.candidate);
    });
    right.addEventListener("icecandidate", (event) => {
      if (event.candidate) void left.addIceCandidate(event.candidate);
    });
    try {
      const accepted = new Promise((resolveAccepted) => {
        right.addEventListener("datachannel", (event) => resolveAccepted(event.channel), {
          once: true,
        });
      });
      const source = left.createDataChannel("netsu-data/0", {
        ordered: true,
        protocol: api.SUBPROTOCOL,
      });
      const offer = await left.createOffer();
      await left.setLocalDescription(offer);
      await right.setRemoteDescription(left.localDescription);
      const answer = await right.createAnswer();
      await right.setLocalDescription(answer);
      await left.setRemoteDescription(right.localDescription);
      const sink = await accepted;
      sink.binaryType = "arraybuffer";
      await Promise.all([api.waitForChannelOpen(source), api.waitForChannelOpen(sink)]);
      let received = 0;
      const eof = new Promise((resolveEof, rejectEof) => {
        sink.addEventListener("message", (event) => {
          if (!(event.data instanceof ArrayBuffer)) rejectEof(new Error("received text"));
          else if (event.data.byteLength === 0) resolveEof();
          else received += event.data.byteLength;
        });
      });
      const payload = crypto.getRandomValues(new Uint8Array(3 * 16 * 1024 + 17));
      await api.sendBytes(source, payload);
      await api.drainAndFinish(source);
      await eof;
      return {
        sent: payload.byteLength,
        received,
        label: sink.label,
        protocol: sink.protocol,
        ordered: sink.ordered,
      };
    } finally {
      left.close();
      right.close();
    }
  });
  expect(result.sent).toBe(3 * 16 * 1024 + 17);
  expect(result.received).toBe(result.sent);
  expect(result).toMatchObject({
    label: "netsu-data/0",
    protocol: "netsu/iperf3-webrtc/1",
    ordered: true,
  });
});

test("backpressure waits for the low watermark before fragmenting", async ({ page }) => {
  const result = await page.evaluate(async () => {
    const api = globalThis.NetsuBrowserPeer.internals;
    const listeners = new Map();
    const channel = {
      bufferedAmount: 5 * 1024 * 1024,
      bufferedAmountLowThreshold: 0,
      readyState: "open",
      chunks: [],
      addEventListener(name, listener) {
        listeners.set(name, listener);
        if (name === "bufferedamountlow") {
          setTimeout(() => {
            this.bufferedAmount = 0;
            listener();
          }, 0);
        }
      },
      removeEventListener(name, listener) {
        if (listeners.get(name) === listener) listeners.delete(name);
      },
      send(bytes) {
        this.chunks.push(bytes.byteLength);
      },
    };
    await api.sendBytes(channel, new Uint8Array(2 * 16 * 1024 + 7));
    return {
      threshold: channel.bufferedAmountLowThreshold,
      chunks: channel.chunks,
      listeners: listeners.size,
    };
  });
  expect(result.threshold).toBe(1024 * 1024);
  expect(result.chunks).toEqual([16 * 1024, 16 * 1024, 7]);
  expect(result.listeners).toBe(0);
});

test("configuration rejects TURN, malformed URLs, and invalid signaling input", async ({ page }) => {
  const failures = await page.evaluate(() => {
    const api = globalThis.NetsuBrowserPeer.internals;
    const capture = (operation) => {
      try {
        operation();
      } catch (error) {
        return { kind: error.kind, message: error.message };
      }
      return null;
    };
    return [
      capture(() =>
        api.validateConfig({
          code: "2345-6789",
          signalUrl: "https://signal.example/v1/signal",
          stunUrls: ["turn:relay.example"],
        }),
      ),
      capture(() =>
        api.validateConfig({ code: "2345-6789", signalUrl: "not a URL", stunUrls: [] }),
      ),
      capture(() => api.validateSignal({ v: 1, type: "description", sdp_type: "offer" })),
      capture(() => api.makeBind("listener", "")),
    ];
  });
  expect(failures.map((failure) => failure.kind)).toEqual([
    "config",
    "config",
    "signaling",
    "signaling",
  ]);
});

test("missing payload channels fail before throughput", async ({ page }) => {
  const failure = await page.evaluate(() => {
    const peer = globalThis.NetsuBrowserPeer;
    try {
      peer.internals.validateChannelManifest(
        [
          {
            label: "netsu-control",
            protocol: "netsu/iperf3-webrtc/1",
            ordered: true,
            maxPacketLifeTime: null,
            maxRetransmits: null,
            negotiated: false,
          },
        ],
        1,
      );
    } catch (error) {
      return peer.errorResult(error);
    }
    return null;
  });
  expect(failure.error.kind).toBe("channels_missing");
  expect(JSON.stringify(failure)).not.toContain("bits_per_second");
});

test("relay and missing candidate pairs are direct-path failures", async ({ page }) => {
  const failures = await page.evaluate(() => {
    const peer = globalThis.NetsuBrowserPeer;
    const cases = [
      new Map([
        ["transport", { type: "transport", selectedCandidatePairId: "pair" }],
        [
          "pair",
          {
            type: "candidate-pair",
            state: "succeeded",
            nominated: true,
            localCandidateId: "local",
            remoteCandidateId: "remote",
          },
        ],
        ["local", { type: "local-candidate", candidateType: "host", protocol: "udp" }],
        ["remote", { type: "remote-candidate", candidateType: "relay", protocol: "udp" }],
      ]),
      new Map(),
    ];
    return cases.map((reports) => {
      try {
        peer.internals.directPairFromStats(reports, false);
      } catch (error) {
        return peer.errorResult(error);
      }
      return null;
    });
  });
  for (const failure of failures) {
    expect(failure.error.kind).toBe("direct_path_unavailable");
    expect(failure.error.message).toContain("no throughput test was run");
    expect(JSON.stringify(failure)).not.toContain("bits_per_second");
  }
});

test("missing DataChannel open fails before any throughput result", async ({ page }) => {
  const failure = await page.evaluate(async () => {
    const peer = globalThis.NetsuBrowserPeer;
    const channel = { readyState: "connecting", addEventListener() {} };
    try {
      await peer.internals.waitForChannelOpen(channel, 20);
    } catch (error) {
      return peer.errorResult(error);
    }
    return null;
  });
  expect(failure.error.kind).toBe("setup_timeout");
  expect(JSON.stringify(failure)).not.toContain("bits_per_second");
});

test("DataChannel metadata must be reliable, ordered, in-band, and namespaced", async ({
  page,
}) => {
  const result = await page.evaluate(() => {
    const api = globalThis.NetsuBrowserPeer.internals;
    const valid = {
      label: "netsu-control",
      protocol: api.SUBPROTOCOL,
      ordered: true,
      maxPacketLifeTime: null,
      maxRetransmits: null,
      negotiated: false,
    };
    api.validateDataChannel(valid, "netsu-control");
    try {
      api.validateDataChannel({ ...valid, ordered: false }, "netsu-control");
    } catch (error) {
      return { validAccepted: true, invalidKind: error.kind };
    }
    return { validAccepted: true, invalidKind: null };
  });
  expect(result).toEqual({ validAccepted: true, invalidKind: "protocol_error" });
});
