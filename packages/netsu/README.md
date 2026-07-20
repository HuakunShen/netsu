# netsu

iperf3-compatible network speed test — TypeScript library and CLI.

- Speaks the **iperf3 wire protocol**: test against an official `iperf3 -s`,
  or point `iperf3 -c` at a netsu server.
- Adds a **WebSocket mode** (netsu ↔ netsu only) that traverses HTTP proxies.
- TCP / UDP / WS × upload / reverse, parallel streams, interval reports,
  UDP jitter & loss, `--json` output.

## CLI

```bash
# server
npx netsu server -p 5201            # tcp+udp, iperf3-compatible
npx netsu server -p 5201 --ws       # websocket mode

# client
npx netsu client <host> -t 5                 # tcp upload
npx netsu client <host> -R                   # reverse: server sends
npx netsu client <host> -u -b 10M            # udp at 10 Mbit/s
npx netsu client <host> --ws -P 4            # websocket, 4 streams
npx netsu client <host> --json               # machine-readable output
```

Works against official iperf3 either way:

```bash
iperf3 -s -p 5201            # official server …
npx netsu client localhost   # … netsu client

npx netsu server -p 5201     # netsu server …
iperf3 -c localhost          # … official client
```

## Library

```ts
import { runClient, startServer } from "netsu";

const server = await startServer({ port: 5201 });

const result = await runClient("127.0.0.1", {
  port: 5201,
  duration: 5,
  reverse: false,
  parallel: 2,
  onInterval: (r) => console.log(r.bitsPerSecond),
});
console.log(result.sendBitsPerSecond, result.receiveBitsPerSecond);

await server.close();
```

The wire protocol is documented in
[PROTOCOL.md](https://github.com/HuakunShen/netsu/blob/main/PROTOCOL.md).
