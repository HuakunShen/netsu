import * as net from "net";

export interface SpeedTestServer {
  stop: () => void;
}

export interface SpeedTestOptions {
  duration?: number; // Test duration in milliseconds
  chunkSize?: number; // Chunk size in bytes
  port?: number; // Port number
  onProgress?: (speed: number) => void; // Callback for progress updates
}

export interface SpeedTestResult {
  bytesTransferred: number;
  duration: number; // in seconds
  speed: number; // in Mbps
}

/**
 * Start a speed test server
 * @param options Server configuration options
 * @returns Object containing server control methods
 */
export function startServer(options: SpeedTestOptions = {}): SpeedTestServer {
  const TEST_DURATION = options.duration || 10000;
  const CHUNK_SIZE = options.chunkSize || 1024 * 1024;
  const port = options.port || 5201;

  const chunk = Buffer.from(new Uint8Array(CHUNK_SIZE));
  chunk.fill("x");

  const server = net.createServer((socket) => {
    console.log("Client connected");

    const startTime = Date.now();
    let bytesSent = 0;

    const sendData = () => {
      if (Date.now() - startTime < TEST_DURATION) {
        while (socket.writable && Date.now() - startTime < TEST_DURATION) {
          const canWrite = socket.write(new Uint8Array(chunk));
          bytesSent += CHUNK_SIZE;

          if (!canWrite) {
            return;
          }
        }
        setTimeout(sendData, 0);
      } else {
        const duration = (Date.now() - startTime) / 1000;
        const speedMbps = (bytesSent * 8) / (1000000 * duration);

        if (options.onProgress) {
          options.onProgress(speedMbps);
        }

        socket.end();
      }
    };

    socket.on("drain", sendData);
    socket.on("error", (err) => console.error("Socket error:", err));
    sendData();
  });

  server.listen(port);
  console.log(`Speed test server running on port ${port}`);

  return {
    stop: () => server.close(),
  };
}

/**
 * Run a speed test client
 * @param host Server hostname or IP
 * @param options Client configuration options
 * @returns Promise that resolves with test results
 */
export function runClient(
  host: string,
  options: SpeedTestOptions = {}
): Promise<SpeedTestResult> {
  const port = options.port || 5201;

  return new Promise((resolve, reject) => {
    const socket = new net.Socket();
    const startTime = Date.now();
    let bytesReceived = 0;

    socket.connect(port, host, () => {
      console.log("Connected to server");
    });

    socket.on("data", (data: Buffer) => {
      bytesReceived += data.length;

      if (options.onProgress) {
        const elapsed = (Date.now() - startTime) / 1000;
        const currentSpeed = (bytesReceived * 8) / (1000000 * elapsed);
        options.onProgress(currentSpeed);
      }
    });

    socket.on("end", () => {
      const duration = (Date.now() - startTime) / 1000;
      const speed = (bytesReceived * 8) / (1000000 * duration);

      socket.destroy();
      resolve({
        bytesTransferred: bytesReceived,
        duration,
        speed,
      });
    });

    socket.on("error", (err) => {
      socket.destroy();
      reject(err);
    });
  });
}
