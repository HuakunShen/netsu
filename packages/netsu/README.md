# netsu

- NPM: https://www.npmjs.com/package/netsu?activeTab=readme
- JSR: https://jsr.io/@hk/netsu

This package is a library and CLI for testing the speed of your network. Similar to iperf3.

## CLI

```bash
npx netsu server --port 5201 --protocol tcp

npx netsu client --host <server-ip> --port 5201 --protocol tcp --type download --duration 2
```

## Library

```ts
import { runClient, startServer } from "netsu";

// on server
startServer({ port: 5201, protocol: "tcp" });

// on client
const testResult = await runClient({
  host: "127.0.0.1", // replace with server ip
  port: 5201,
  protocol: "tcp",
  type: "download",
});
```
