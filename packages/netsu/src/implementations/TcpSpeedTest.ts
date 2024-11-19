import * as net from "net";
import { SpeedTestBase } from "../base/SpeedTestBase";
import type { SpeedTestOptions, SpeedTestResult } from "../types";

export class TcpServer extends SpeedTestBase {
  private server: net.Server;

  constructor(options: SpeedTestOptions) {
    super(options);
    this.server = net.createServer();
  }

  async start(): Promise<void> {
    this.server = net.createServer((socket) => {
      this.startTime = Date.now();
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
    const chunk = this.createChunk();

    if (this.options.testType === "download") {
      const sendData = () => {
        if (Date.now() - this.startTime < this.options.duration) {
          while (
            socket.writable &&
            Date.now() - this.startTime < this.options.duration
          ) {
            const canWrite = socket.write(new Uint8Array(chunk));
            this.bytesTransferred += chunk.length;
            this.reportProgress();
            if (!canWrite) return;
          }
          setTimeout(sendData, 0);
        } else {
          socket.end();
        }
      };

      socket.on("drain", sendData);
      sendData();
    } else {
      socket.on("data", (data: Buffer) => {
        this.bytesTransferred += data.length;
        this.reportProgress();
      });
    }

    socket.on("error", (err) => console.error("Socket error:", err));
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

      this.socket.on("error", reject);
    });
  }

  private startUpload(): void {
    const chunk = this.createChunk();
    const sendData = () => {
      if (Date.now() - this.startTime < this.options.duration) {
        while (
          this.socket.writable &&
          Date.now() - this.startTime < this.options.duration
        ) {
          const canWrite = this.socket.write(new Uint8Array(chunk));
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
