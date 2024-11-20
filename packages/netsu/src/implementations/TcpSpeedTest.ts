import * as v from "valibot";
import * as net from "net";
import { SpeedTestServerBase } from "../base/SpeedTestServerBase";
import { SpeedTestClientBase } from "../base/SpeedTestClientBase";
import {
  TestMessage,
  type SpeedTestClientOptions,
  type SpeedTestResult,
  type SpeedTestServerOptions,
} from "../types";
import { createChunk } from "../utils";

export class TcpServer extends SpeedTestServerBase {
  private server: net.Server;
  private connections: Map<
    net.Socket,
    {
      chunkSize: number;
      startTime: number;
      bytesTransferred: number;
    }
  > = new Map();

  constructor(options: SpeedTestServerOptions) {
    super(options);
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
    const connection = {
      chunkSize: 0,
      startTime: Date.now(),
      bytesTransferred: 0,
    };
    this.connections.set(socket, connection);

    socket.once("data", (data) => {
      try {
        const message: TestMessage = v.parse(
          TestMessage,
          JSON.parse(data.toString())
        );
        if (message.type === "start") {
          testType = message.testType;

          if (testType === "download") {
            this.startDownloadTest(socket);
          }
        }
      } catch (err) {
        console.error("Invalid start message");
        socket.end();
      }
    });

    socket.on("data", (data) => {
      if (testType === "upload") {
        connection.bytesTransferred += data.length;
        const speed = this.calculateSpeed(
          connection.bytesTransferred,
          Date.now() - connection.startTime
        );
        this.options.onProgress?.(speed);
      }
    });
    socket.on("close", () => {
      console.log("Client disconnected", socket.address());
    });
    socket.on("error", (err) => {});
  }

  private startDownloadTest(socket: net.Socket): void {
    const connection = this.connections.get(socket);
    if (!connection) return;
    const chunk = createChunk(connection.chunkSize);

    const sendData = () => {
      while (socket.writable) {
        const canWrite = socket.write(chunk);
        connection.bytesTransferred += chunk.length;
        const speed = this.calculateSpeed(
          connection.bytesTransferred,
          Date.now() - connection.startTime
        );
        this.options.onProgress?.(speed);

        if (!canWrite) return;
      }
      sendData();
    };

    socket.on("drain", sendData);
    sendData();
  }

  stop(): void {
    this.server.close();
  }
}

export class TcpClient extends SpeedTestClientBase {
  private socket: net.Socket;

  constructor(
    private host: string,
    options: SpeedTestClientOptions
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
        const result = this.getResult(
          this.options.protocol,
          this.options.testType
        );
        this.socket.destroy();
        resolve(result);
      });

      this.socket.on("error", (err) => {
        this.socket.destroy();
        reject(err);
      });

      // Set up a timer to end the test
      setTimeout(() => {
        const result = this.getResult(
          this.options.protocol,
          this.options.testType
        );
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
