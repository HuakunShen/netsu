import { WebSocket, WebSocketServer as WSServer } from "ws";
import { SpeedTestBase } from "../base/SpeedTestBase";
import type { SpeedTestOptions, SpeedTestResult } from "../types";

export class WebSocketServer extends SpeedTestBase {
  private server?: WSServer;
  private connections: Set<WebSocket>;

  constructor(options: SpeedTestOptions) {
    super(options);
    this.connections = new Set();
  }

  async start(): Promise<void> {
    return new Promise((resolve) => {
      this.server = new WSServer({ port: this.options.port });
      this.startTime = Date.now();

      this.server.on("connection", (ws) => {
        console.log("WebSocket client connected");
        this.connections.add(ws);
        this.handleConnection(ws);

        ws.on("close", () => {
          this.connections.delete(ws);
        });
      });

      this.server.on("error", (err) => {
        console.error("WebSocket Server error:", err);
      });

      this.server.on("listening", () => {
        console.log(`WebSocket server listening on port ${this.options.port}`);
        resolve();
      });
    });
  }

  private handleConnection(ws: WebSocket): void {
    const chunk = this.createChunk();

    if (this.options.testType === "download") {
      const sendData = () => {
        if (
          Date.now() - this.startTime < this.options.duration &&
          ws.readyState === WebSocket.OPEN
        ) {
          ws.send(chunk, (err) => {
            if (err) {
              console.error("Error sending data:", err);
              return;
            }
            this.bytesTransferred += chunk.length;
            this.reportProgress();
            setImmediate(sendData);
          });
        }
      };

      sendData();
    } else {
      ws.on("message", (data: Buffer) => {
        this.bytesTransferred += data.length;
        this.reportProgress();
        ws.send("ACK");
      });
    }
  }

  stop(): void {
    for (const ws of this.connections) {
      ws.close();
    }
    this.server?.close();
  }
}

export class WebSocketClient extends SpeedTestBase {
  private ws?: WebSocket;
  private isRunning: boolean = false;

  constructor(
    private host: string,
    options: SpeedTestOptions
  ) {
    super(options);
  }

  private startUpload(): void {
    const chunk = this.createChunk();
    const sendData = () => {
      if (!this.isRunning || !this.ws || this.ws.readyState !== WebSocket.OPEN)
        return;

      this.ws.send(chunk, (err) => {
        if (err) {
          console.error("Error sending data:", err);
          return;
        }
        this.bytesTransferred += chunk.length;
        this.reportProgress();

        // Schedule next send after current send completes
        if (
          this.isRunning &&
          Date.now() - this.startTime < this.options.duration
        ) {
          setImmediate(sendData);
        }
      });
    };

    sendData();
  }

  async start(): Promise<SpeedTestResult> {
    return new Promise((resolve, reject) => {
      const wsUrl = `ws://${this.host}:${this.options.port}`;
      this.ws = new WebSocket(wsUrl);
      this.startTime = Date.now();
      this.isRunning = true;

      this.ws.on("open", () => {
        console.log("Connected to WebSocket server");
        if (this.options.testType === "upload") {
          this.startUpload();
        }
      });

      this.ws.on("message", (data: Buffer) => {
        if (this.options.testType === "download") {
          this.bytesTransferred += data.length;
          this.reportProgress();
        }
      });

      // Set up a timer to end the test
      setTimeout(() => {
        const result = this.getResult();
        this.stop();
        resolve(result);
      }, this.options.duration);
    });
  }

  stop(): void {
    this.isRunning = false;
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.close();
    }
  }
}
