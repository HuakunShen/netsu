
import { startServer, runClient } from "./src/speed-test";

// Server example
async function runServer() {
  const server = startServer({
    port: 5201,
    duration: 10000,
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

// Client example
async function runTest() {
  try {
    const result = await runClient("localhost", {
      port: 5201,
      onProgress: (speed) => {
        console.log(`Current speed: ${speed.toFixed(2)} Mbps`);
      },
    });

    console.log("\nTest Results:");
    console.log(`Bytes received: ${result.bytesTransferred}`);
    console.log(`Duration: ${result.duration.toFixed(2)} seconds`);
    console.log(`Average speed: ${result.speed.toFixed(2)} Mbps`);
  } catch (error) {
    console.error("Test failed:", error);
  }
}

// Run server or client based on command line argument
if (process.argv[2] === "server") {
  runServer();
} else if (process.argv[2] === "client") {
  runTest();
} else {
  console.log("Usage: ts-node example.ts [server|client]");
}
