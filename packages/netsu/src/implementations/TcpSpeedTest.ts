import * as v from "valibot";
import * as net from "net";
import { SpeedTestBase } from "../base/SpeedTestBase";
import {
  TestMessage,
  type SpeedTestOptions,
  type SpeedTestResult,
} from "../types";

export class TcpServer extends SpeedTestBase {
  private server: net.Server;

  constructor(options: Omit<SpeedTestOptions, "testType">) {
    super({ ...options, testType: "download" }); // Default value, but won't be used
    this.server = net.createServer();
  }

  async start(): Promise<void> {
    this.server = net.createServer((socket) => {
      console.log("Client connected", socket.address());
      this.handleConnection(socket);
    });

    return new Promise((resolve) => {
      this.server.listen(this.options.port, () => {
        console.log(`TCP server listening on port ${this.options.port}`);
        resolve();
      });
    });
  }

  private handleConnection(socket: net.Socket): void {
    let testType: "upload" | "download" | undefined;
    let startTime = 0;
    let bytesTransferred = 0;

    socket.once("data", (data) => {
      try {
        const message: TestMessage = v.parse(
          TestMessage,
          JSON.parse(data.toString())
        );
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
        socket.end();
      }
    });

    socket.on("data", (data) => {
      if (testType === "upload") {
        bytesTransferred += data.length;
        const speed = this.calculateSpeed(
          bytesTransferred,
          Date.now() - startTime
        );
        this.options.onProgress?.(speed);
      }
    });
    socket.on("close", () => {
      console.log("Client disconnected", socket.address());
    });
    socket.on("error", (err) => {});
  }

  private startDownloadTest(
    socket: net.Socket,
    bytesTransferred: number,
    startTime: number
  ): void {
    const chunk = new Uint8Array(this.createChunk());

    const sendData = () => {
      if (Date.now() - startTime < this.options.duration) {
        while (
          socket.writable &&
          Date.now() - startTime < this.options.duration
        ) {
          const canWrite = socket.write(chunk);
          bytesTransferred += chunk.length;
          const speed = this.calculateSpeed(
            bytesTransferred,
            Date.now() - startTime
          );
          this.options.onProgress?.(speed);

          if (!canWrite) return;
        }
        setTimeout(sendData, 0);
      } else {
        socket.end();
      }
    };

    socket.on("drain", sendData);
    sendData();
  }

  stop(): void {
    this.server.close();
  }
}

export class TcpClient extends SpeedTestBase {
  private socket: net.Socket;

  constructor(
    private host: string,
    options: SpeedTestOptions
  ) {
    super(options);
    this.socket = new net.Socket();
  }

  async start(): Promise<SpeedTestResult> {
    return new Promise((resolve, reject) => {
      this.startTime = Date.now();

      this.socket.connect(this.options.port, this.host, () => {
        // Send start message with chunk size if specified
        const startMessage: TestMessage = {
          type: "start",
          testType: this.options.testType,
          chunkSize: this.options.chunkSize,
        };
        this.socket.write(JSON.stringify(startMessage));

        if (this.options.testType === "upload") {
          this.startUpload();
        }
      });

      this.socket.on("data", (data) => {
        if (this.options.testType === "download") {
          this.bytesTransferred += data.length;
          this.reportProgress();
        }
      });

      this.socket.on("end", () => {
        const result = this.getResult();
        this.socket.destroy();
        resolve(result);
      });

      this.socket.on("error", (err) => {
        this.socket.destroy();
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
          this.socket.writable &&
          Date.now() - this.startTime < this.options.duration
        ) {
          const canWrite = this.socket.write(chunk);
          this.bytesTransferred += chunk.length;
          this.reportProgress();
          if (!canWrite) return;
        }
        setTimeout(sendData, 0);
      } else {
        this.socket.end();
      }
    };

    this.socket.on("drain", sendData);
    sendData();
  }

  stop(): void {
    this.socket.destroy();
  }
}
