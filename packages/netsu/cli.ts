#!/usr/bin/env node
import { defineCommand, runMain } from "citty";
import * as v from "valibot";
import { runClient, startServer } from "./src/speed-test";

const NumberSchema = v.pipe(v.unknown(), v.transform(Number));

const ServerArgsSchema = v.object({
  port: NumberSchema,
  //   duration: NumberSchema,
  protocol: v.union([
    v.literal("tcp"),
    v.literal("udp"),
    v.literal("websocket"),
    v.literal("ws"),
  ]),
});

const serverCmd = defineCommand({
  meta: {
    name: "server",
    description: "Start a speed test server",
  },
  args: {
    protocol: {
      type: "string",
      description: "Protocol to use (tcp, udp, websocket, or ws)",
      required: false,
      default: "tcp",
    },
    port: {
      type: "string",
      description: "Port server listen on",
      required: false,
      default: "5201",
    },
  },
  run({ args }) {
    const result = v.safeParse(ServerArgsSchema, args);
    if (!result.success) {
      console.error(v.flatten(result.issues));
      process.exit(1);
    }

    const { port, protocol } = result.output;
    startServer({
      port,
      protocol: protocol === "ws" ? "websocket" : protocol,
      // onProgress: (speed) => {
      //   if (Date.now() % 500 === 0) {
      //     // Print every 500ms (2 times per second)
      //     process.stdout.write(
      //       `\rCurrent server speed: ${speed.toFixed(2)} Mbps`
      //     );
      //   }
      // },
    });
  },
});

const ClientArgsSchema = v.object({
  host: v.string(),
  type: v.union([v.literal("upload"), v.literal("download")]),
  port: NumberSchema,
  duration: NumberSchema,
  protocol: v.union([
    v.literal("tcp"),
    v.literal("udp"),
    v.literal("websocket"),
    v.literal("ws"),
  ]),
  chunkSize: NumberSchema,
});

const clientCmd = defineCommand({
  meta: {
    name: "client",
    description: "Run a speed test client",
  },
  args: {
    host: {
      type: "string",
      description: "Host to connect to",
      required: true,
    },
    type: {
      type: "string",
      description: "Test type (upload or download)",
      required: false,
      default: "download",
    },
    duration: {
      type: "string",
      description: "Duration of the test in seconds",
      required: false,
      default: "3",
    },
    protocol: {
      type: "string",
      description: "Protocol to use (tcp, udp, websocket, or ws)",
      required: false,
      default: "tcp",
    },
    port: {
      type: "string",
      description: "Port server listen on",
      required: false,
      default: "5201",
    },
    chunkSize: {
      type: "string",
      description: "Size of data chunks in bytes",
      required: false,
      default: "1048576", // 1MB (1024 * 1024)
    },
  },
  async run({ args }) {
    const result = v.safeParse(ClientArgsSchema, args);
    if (!result.success) {
      console.error(v.flatten(result.issues));
      process.exit(1);
    }

    const { host, type, port, duration, protocol, chunkSize } = result.output;
    const testResult = await runClient(host, {
      port,
      duration: duration * 1000,
      protocol: protocol === "ws" ? "websocket" : protocol,
      testType: type,
      chunkSize: chunkSize ? chunkSize : undefined,
      onProgress: (speed) => {
        if (Date.now() % 500 === 0) {
          process.stdout.write(
            `\rCurrent client speed: ${speed.toFixed(2)} Mbps`
          );
        }
      },
    });
    console.log("\nTest Results:");
    console.log(`Protocol: ${testResult.protocol}`);
    console.log(`Test type: ${testResult.testType}`);
    console.log(`Bytes transferred: ${testResult.bytesTransferred}`);
    console.log(`Duration: ${testResult.duration.toFixed(2)} seconds`);
    console.log(`Average speed: ${testResult.speed.toFixed(2)} Mbps`);
  },
});

const mainCmd = defineCommand({
  meta: {
    name: "netsu",
    description: "A speed test tool",
  },
  subCommands: {
    server: serverCmd,
    client: clientCmd,
  },
});

await runMain(mainCmd);
