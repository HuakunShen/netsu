import * as dgram from "dgram";
import { SpeedTestBase } from "../base/SpeedTestBase";
import type { SpeedTestOptions, SpeedTestResult } from "../types";

export class UdpServer extends SpeedTestBase {
  private server: dgram.Socket;
  private clients: Map<string, { port: number; address: string }>;

  constructor(options: SpeedTestOptions) {
    super(options);
    this.server = dgram.createSocket("udp4");
    this.clients = new Map();
  }

  async start(): Promise<void> {
    return new Promise((resolve) => {
      this.server.on("message", (msg, rinfo) => {
        this.handleMessage(msg, rinfo);
      });

      this.server.on("error", (err) => {
        console.error("UDP Server error:", err);
      });

      this.server.bind(this.options.port, () => {
        console.log(`UDP server listening on port ${this.options.port}`);
        this.startTime = Date.now();
        resolve();
      });
    });
  }

  private handleMessage(msg: Buffer, rinfo: dgram.RemoteInfo): void {
    const clientKey = `${rinfo.address}:${rinfo.port}`;

    if (!this.clients.has(clientKey)) {
      this.clients.set(clientKey, { port: rinfo.port, address: rinfo.address });
    }

    if (this.options.testType === "upload") {
      this.bytesTransferred += msg.length;
      this.reportProgress();

      // Send small acknowledgment back
      const ack = new Uint8Array(Buffer.from("ack"));
      this.server.send(ack, rinfo.port, rinfo.address);
    } else {
      // For download test, send data chunks to client
      const chunk = new Uint8Array(this.createChunk());
      this.server.send(chunk, rinfo.port, rinfo.address);
      this.bytesTransferred += chunk.length;
      this.reportProgress();
    }
  }

  stop(): void {
    this.server.close();
  }
}

export class UdpClient extends SpeedTestBase {
  private client: dgram.Socket;
  private isRunning: boolean = false;

  constructor(
    private host: string,
    options: SpeedTestOptions,
  ) {
    super(options);
    this.client = dgram.createSocket("udp4");
  }

  async start(): Promise<SpeedTestResult> {
    return new Promise((resolve, reject) => {
      this.startTime = Date.now();
      this.isRunning = true;

      this.client.on("error", (err) => {
        this.isRunning = false;
        reject(err);
      });

      this.client.on("message", (msg) => {
        if (this.options.testType === "download") {
          this.bytesTransferred += msg.length;
          this.reportProgress();
        }
      });

      // Set up a timer to end the test
      setTimeout(() => {
        this.isRunning = false;
        const result = this.getResult();
        this.stop();
        resolve(result);
      }, this.options.duration);

      if (this.options.testType === "upload") {
        this.startUpload();
      } else {
        // Initiate download by sending a small message
        const startMsg = new Uint8Array(Buffer.from("start"));
        this.client.send(startMsg, this.options.port, this.host);
      }
    });
  }

  private startUpload(): void {
    const chunk = new Uint8Array(this.createChunk());
    const sendData = () => {
      if (!this.isRunning) return;

      this.client.send(chunk, this.options.port, this.host, (err) => {
        if (err) {
          console.error("UDP send error:", err);
          return;
        }

        this.bytesTransferred += chunk.length;
        this.reportProgress();

        if (this.isRunning) {
          setTimeout(sendData, 0);
        }
      });
    };

    sendData();
  }

  stop(): void {
    this.isRunning = false;
    this.client.close();
  }
}
