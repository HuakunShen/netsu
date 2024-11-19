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
          ws.send(chunk);
          this.bytesTransferred += chunk.length;
          this.reportProgress();
          setTimeout(sendData, 0);
        }
      };

      sendData();
    } else {
      ws.on("message", (data: Buffer) => {
        this.bytesTransferred += data.length;
        this.reportProgress();
      });
    }

    ws.on("error", (err) => {
      console.error("WebSocket connection error:", err);
    });
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

      this.ws.on("error", (err) => {
        this.isRunning = false;
        reject(err);
      });

      this.ws.on("close", () => {
        this.isRunning = false;
      });

      // Set up a timer to end the test
      setTimeout(() => {
        const result = this.getResult();
        this.stop();
        resolve(result);
      }, this.options.duration);
    });
  }

  private startUpload(): void {
    const chunk = this.createChunk();
    const sendData = () => {
      if (!this.isRunning || !this.ws || this.ws.readyState !== WebSocket.OPEN)
        return;

      this.ws.send(chunk);
      this.bytesTransferred += chunk.length;
      this.reportProgress();

      // Schedule next send
      if (
        this.isRunning &&
        Date.now() - this.startTime < this.options.duration
      ) {
        setTimeout(sendData, 0);
      }
    };

    sendData();
  }

  stop(): void {
    this.isRunning = false;
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.close();
    }
  }
}
