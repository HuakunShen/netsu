import process from "node:process";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";

function parseArgs(argv) {
  const config = { stunUrls: [], reverse: false, includeAddresses: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const value = () => {
      const next = argv[++index];
      if (!next) throw new Error(`${arg} requires a value`);
      return next;
    };
    if (arg === "--code") config.code = value();
    else if (arg === "--signal-url") config.signalUrl = value();
    else if (arg === "--stun") config.stunUrls.push(value());
    else if (arg === "--parallel") config.parallel = Number(value());
    else if (arg === "--duration") config.duration = Number(value());
    else if (arg === "--length") config.length = Number(value());
    else if (arg === "--reverse") config.reverse = true;
    else if (arg === "--include-addresses") config.includeAddresses = true;
    else throw new Error(`unknown argument ${arg}`);
  }
  if (!config.code || !config.signalUrl) {
    throw new Error("--code and --signal-url are required");
  }
  return config;
}

let browser;
try {
  const config = parseArgs(process.argv.slice(2));
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext();
  const page = await context.newPage();
  page.on("pageerror", (error) => console.error(`browser page error: ${error.message}`));
  page.on("websocket", (socket) => {
    socket.on("socketerror", (error) =>
      console.error(`browser signaling WebSocket error: ${String(error)}`),
    );
  });
  const bootstrapUrl = new URL(config.signalUrl);
  bootstrapUrl.pathname = "/healthz";
  bootstrapUrl.search = "";
  bootstrapUrl.hash = "";
  await page.goto(bootstrapUrl.toString());
  await page.addScriptTag({
    path: fileURLToPath(new URL("./browser-peer.js", import.meta.url)),
  });
  const outcome = await page.evaluate(async (input) => {
    try {
      return { ok: true, result: await globalThis.NetsuBrowserPeer.run(input) };
    } catch (error) {
      return { ok: false, failure: globalThis.NetsuBrowserPeer.errorResult(error) };
    }
  }, config);
  if (!outcome.ok) {
    const error = new Error(outcome.failure.error.message);
    error.kind = outcome.failure.error.kind;
    error.failure = outcome.failure;
    throw error;
  }
  process.stdout.write(`${JSON.stringify(outcome.result)}\n`);
} catch (error) {
  const fallback = error?.failure ?? {
    error: {
      transport: "webrtc",
      kind: error?.kind ?? "runtime_error",
      message: String(error?.message ?? error),
    },
  };
  process.stdout.write(`${JSON.stringify(fallback)}\n`);
  if (fallback.error.kind === "direct_path_unavailable") {
    console.error(
      "warning: WebRTC direct connection failed; netsu does not use TURN relay, so no throughput test was run",
    );
    process.exitCode = 4;
  } else if (fallback.error.kind === "setup_timeout") {
    process.exitCode = 3;
  } else if (fallback.error.kind === "config") {
    process.exitCode = 2;
  } else {
    process.exitCode = 1;
  }
} finally {
  await browser?.close();
}
