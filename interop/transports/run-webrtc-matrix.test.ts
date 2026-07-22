import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";

import { describe, expect, test } from "vitest";

import {
  buildUdpBlockRule,
  buildIsolatedCaseEnvironments,
  buildWebRtcMatrix,
  observeChildClose,
  peerServicesForCell,
  redactArtifact,
} from "./run-webrtc-matrix";

describe("WebRTC container matrix", () => {
  test("covers Rust and Chromium direct paths plus bounded blocked paths", () => {
    expect(buildWebRtcMatrix()).toEqual([
      {
        name: "rust-upload-p1",
        peer: "rust",
        direction: "upload",
        parallel: 1,
        blocked: false,
      },
      {
        name: "rust-upload-p4",
        peer: "rust",
        direction: "upload",
        parallel: 4,
        blocked: false,
      },
      {
        name: "rust-reverse-p1",
        peer: "rust",
        direction: "reverse",
        parallel: 1,
        blocked: false,
      },
      {
        name: "rust-reverse-p4",
        peer: "rust",
        direction: "reverse",
        parallel: 4,
        blocked: false,
      },
      {
        name: "chromium-upload-p1",
        peer: "chromium",
        direction: "upload",
        parallel: 1,
        blocked: false,
      },
      {
        name: "chromium-upload-p4",
        peer: "chromium",
        direction: "upload",
        parallel: 4,
        blocked: false,
      },
      {
        name: "chromium-reverse-p1",
        peer: "chromium",
        direction: "reverse",
        parallel: 1,
        blocked: false,
      },
      {
        name: "rust-blocked-upload-p1",
        peer: "rust",
        direction: "upload",
        parallel: 1,
        blocked: true,
      },
      {
        name: "chromium-blocked-upload-p1",
        peer: "chromium",
        direction: "upload",
        parallel: 1,
        blocked: true,
      },
    ]);
  });

  test("default matrix gives every cell its own runner process environment", () => {
    const environments = buildIsolatedCaseEnvironments({ KEEP: "value" });
    expect(environments).toHaveLength(buildWebRtcMatrix().length);
    expect(environments.map((env) => env.WEBRTC_CASE)).toEqual(
      buildWebRtcMatrix().map((cell) => cell.name),
    );
    expect(environments.every((env) => env.KEEP === "value")).toBe(true);
  });

  test("failure artifacts remove secrets, SDP, candidates, and addresses", () => {
    const raw = [
      'listener_secret="do-not-keep"',
      '"secret":"do-not-keep"',
      '"sdp":"v=0\\r\\no=- 1 2 IN IP4 192.0.2.3"',
      "candidate:1 1 UDP 1 192.0.2.4 5000 typ host",
      "local_addr=192.0.2.5:5001",
      "safe line",
    ].join("\n");
    const redacted = redactArtifact(raw);
    expect(redacted).toContain("safe line");
    for (const forbidden of [
      "do-not-keep",
      '"sdp"',
      "candidate:",
      "192.0.2.3",
      "192.0.2.4",
      "192.0.2.5",
    ]) {
      expect(redacted).not.toContain(forbidden);
    }

    const jsonArtifact = redactArtifact(
      JSON.stringify({
        secret: "do-not-keep",
        sdp: "candidate:1 1 UDP 1 192.0.2.4 5000 typ host",
        safe: "kept",
      }),
    );
    expect(JSON.parse(jsonArtifact)).toMatchObject({ safe: "kept" });
  });

  test("container matrix uses the privileged test tier instead of the anonymous rate limit", async () => {
    const [compose, signalImage] = await Promise.all([
      readFile(new URL("./docker-compose.yml", import.meta.url), "utf8"),
      readFile(new URL("./Dockerfile.signal", import.meta.url), "utf8"),
    ]);

    expect(signalImage).not.toContain("netsu-compose-test-token");
    expect(compose.match(/netsu-compose-test-token/g)).toHaveLength(2);
    expect(compose).toContain("API_TOKEN: netsu-compose-test-token");
    expect(compose).toContain("NETSU_SIGNAL_TOKEN: netsu-compose-test-token");
    expect(compose).toContain('CLOUDFLARE_CF_FETCH_ENABLED: "false"');
    expect(
      compose.match(/\$\{COMPOSE_PROJECT_NAME:-netsu-webrtc-e2e\}/g),
    ).toHaveLength(4);
  });

  test("blocked-path injection rejects only UDP to the Rust peer", () => {
    expect(buildUdpBlockRule()).toEqual([
      "-p",
      "udp",
      "-d",
      "rs-server",
      "-j",
      "REJECT",
    ]);
  });

  test("each cell resets exactly the server and active peer container", () => {
    const [rust, chromium] = buildWebRtcMatrix();
    const browser = buildWebRtcMatrix().find(
      (cell) => cell.name === "chromium-upload-p1",
    );

    expect(peerServicesForCell(rust)).toEqual(["rs-server", "rs-client"]);
    expect(browser).toBeDefined();
    expect(peerServicesForCell(browser!)).toEqual(["rs-server", "browser"]);
  });

  test("cell cleanup waits for the host-side compose exec child", async () => {
    const child = spawn(process.execPath, [
      "-e",
      "setTimeout(() => {}, 30000)",
    ]);
    const closed = observeChildClose(child);
    child.kill("SIGTERM");

    await expect(closed).resolves.toBeUndefined();
    expect(child.exitCode !== null || child.signalCode !== null).toBe(true);
  });
});
