import {
  type Protocol,
  type TestType,
  startServer,
  runClient,
} from "./src/speed-test";

async function runServer() {
  const protocol = (process.argv[3] || "tcp") as Protocol;
  const testType = (process.argv[4] || "download") as TestType;

  const server = startServer({
    port: 5201,
    duration: 10000,
    protocol,
    testType,
    onProgress: (speed) => {
      console.log(`Current server speed: ${speed.toFixed(2)} Mbps`);
    },
  });

  // Stop server after 30 seconds
  setTimeout(() => {
    server.stop();
    console.log("Server stopped");
  }, 30000);
}

async function runTest() {
  const protocol = (process.argv[3] || "tcp") as Protocol;
  const testType = (process.argv[4] || "download") as TestType;

  try {
    const result = await runClient("localhost", {
      port: 5201,
      protocol,
      testType,
      onProgress: (speed) => {
        console.log(`Current ${testType} speed: ${speed.toFixed(2)} Mbps`);
      },
    });

    console.log("\nTest Results:");
    console.log(`Protocol: ${result.protocol}`);
    console.log(`Test type: ${result.testType}`);
    console.log(`Bytes transferred: ${result.bytesTransferred}`);
    console.log(`Duration: ${result.duration.toFixed(2)} seconds`);
    console.log(`Average speed: ${result.speed.toFixed(2)} Mbps`);
  } catch (error) {
    console.error("Test failed:", error);
  }
}

if (process.argv[2] === "server") {
  runServer();
} else if (process.argv[2] === "client") {
  runTest();
} else {
  console.log(
    "Usage: ts-node cli.ts [server|client] [tcp|udp|websocket] [upload|download]"
  );
}
