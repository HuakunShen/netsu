import { WebSocketServer as WsServer, WebSocket } from "ws";
import { SpeedTestBase } from "../base/SpeedTestBase";
import type { SpeedTestOptions, SpeedTestResult, TestMessage } from "../types";

export class WebSocketServer extends SpeedTestBase {
  private server: WsServer;

  constructor(options: Omit<SpeedTestOptions, "testType">) {
    super({ ...options, testType: "download" }); // Default value, not used directly
    this.server = new WsServer({ port: options.port });
  }

  async start(): Promise<void> {
    this.server.on("connection", (socket) => {
      console.log("Client connected");
      this.handleConnection(socket);
    });

    return new Promise((resolve) => {
      this.server.on("listening", () => {
        console.log(`WebSocket server listening on port ${this.options.port}`);
        resolve();
      });
    });
  }

  private handleConnection(socket: WebSocket): void {
    let testType: "upload" | "download" | undefined;
    let startTime = 0;
    let bytesTransferred = 0;

    socket.once("message", (data) => {
      try {
        const message: TestMessage = JSON.parse(data.toString());
        if (message.type === "start") {
          testType = message.testType;
          if (message.chunkSize) {
            this.options.chunkSize = message.chunkSize;
          }

          startTime = Date.now();

          if (testType === "download") {
            this.startDownloadTest(socket, bytesTransferred, startTime);
          }
        }
      } catch (err) {
        console.error("Invalid start message");
        socket.close();
      }
    });

    socket.on("message", (data) => {
      if (testType === "upload") {
        bytesTransferred += Buffer.from(data as ArrayBuffer).length;
        const speed = this.calculateSpeed(
          bytesTransferred,
          Date.now() - startTime
        );
        this.options.onProgress(speed);
      }
    });

    socket.on("error", (err) => console.error("Socket error:", err));
  }

  private startDownloadTest(
    socket: WebSocket,
    bytesTransferred: number,
    startTime: number
  ): void {
    const chunk = new Uint8Array(this.createChunk());

    const sendData = () => {
      if (Date.now() - startTime < this.options.duration) {
        while (
          socket.readyState === WebSocket.OPEN &&
          Date.now() - startTime < this.options.duration
        ) {
          socket.send(chunk);
          bytesTransferred += chunk.length;
          const speed = this.calculateSpeed(
            bytesTransferred,
            Date.now() - startTime
          );
          this.options.onProgress(speed);
        }
        setTimeout(sendData, 0);
      } else {
        socket.close();
      }
    };

    sendData();
  }

  stop(): void {
    this.server.close();
  }
}

export class WebSocketClient extends SpeedTestBase {
  private socket: WebSocket;

  constructor(
    private host: string,
    options: SpeedTestOptions
  ) {
    super(options);
    this.socket = new WebSocket(`ws://${host}:${options.port}`);
  }

  async start(): Promise<SpeedTestResult> {
    return new Promise((resolve, reject) => {
      this.startTime = Date.now();

      this.socket.on("open", () => {
        // Send start message with chunk size if specified
        const startMessage: TestMessage = {
          type: "start",
          testType: this.options.testType,
          chunkSize: this.options.chunkSize,
        };
        this.socket.send(JSON.stringify(startMessage));

        if (this.options.testType === "upload") {
          this.startUpload();
        }
      });

      this.socket.on("message", (data) => {
        if (this.options.testType === "download") {
          this.bytesTransferred += Buffer.from(data as ArrayBuffer).length;
          this.reportProgress();
        }
      });

      this.socket.on("close", () => {
        const result = this.getResult();
        this.socket.terminate();
        resolve(result);
      });

      this.socket.on("error", (err) => {
        this.socket.terminate();
        reject(err);
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
    const chunk = new Uint8Array(this.createChunk());
    const sendData = () => {
      if (Date.now() - this.startTime < this.options.duration) {
        while (
          this.socket.readyState === WebSocket.OPEN &&
          Date.now() - this.startTime < this.options.duration
        ) {
          this.socket.send(chunk);
          this.bytesTransferred += chunk.length;
          this.reportProgress();
        }
        setTimeout(sendData, 0);
      } else {
        this.socket.close();
      }
    };

    sendData();
  }

  stop(): void {
    this.socket.terminate();
  }
}
