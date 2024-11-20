import * as dgram from "dgram";
import { SpeedTestBase } from "../base/SpeedTestBase";
import type { SpeedTestOptions, SpeedTestResult, TestMessage } from "../types";

const MAX_UDP_PACKET_SIZE = 1500;
// const MAX_UDP_PACKET_SIZE = 65507;

export class UdpServer extends SpeedTestBase {
  private server: dgram.Socket;

  constructor(options: Omit<SpeedTestOptions, "testType">) {
    super({
      ...options,
      testType: "download",
      // chunkSize: Math.min(
      //   options.chunkSize ?? MAX_UDP_PACKET_SIZE,
      //   MAX_UDP_PACKET_SIZE
      // ),
    }); // Default value, but won't be used
    this.server = dgram.createSocket("udp4");
  }

  async start(): Promise<void> {
    return new Promise((resolve) => {
      this.server.on("message", (data, rinfo) => {
        try {
          const message = JSON.parse(data.toString());
          if (message.type === "start") {
            if (message.chunkSize) {
              this.options.chunkSize = message.chunkSize;
            }
            this.handleTest(message.testType, rinfo);
          }
        } catch (err) {
          console.error("Invalid message received:", err);
        }
      });

      this.server.on("error", (err) => {
        console.error("Server error:", err);
      });

      this.server.bind(this.options.port, () => {
        console.log(`UDP server listening on port ${this.options.port}`);
        resolve();
      });
    });
  }

  private handleTest(
    testType: "upload" | "download",
    rinfo: dgram.RemoteInfo
  ): void {
    const startTime = Date.now();
    let bytesTransferred = 0;

    if (testType === "upload") {
      // For upload tests, just count incoming data
      this.server.on("message", (data) => {
        if (data.toString() !== "start") {
          bytesTransferred += data.length;
          const speed = this.calculateSpeed(
            bytesTransferred,
            Date.now() - startTime
          );
          this.options.onProgress(speed);
        }
      });
    } else if (testType === "download") {
      // For download tests, send data continuously
      this.startDownloadTest(rinfo, bytesTransferred, startTime);
    }
  }

  private startDownloadTest(
    rinfo: dgram.RemoteInfo,
    bytesTransferred: number,
    startTime: number
  ): void {
    const chunk = this.createChunk();

    const sendData = () => {
      if (Date.now() - startTime < this.options.duration) {
        this.server.send(
          new Uint8Array(chunk),
          rinfo.port,
          rinfo.address,
          (err) => {
            if (err) {
              console.error("Error sending data:", err);
              return;
            }
            bytesTransferred += chunk.length;
            const speed = this.calculateSpeed(
              bytesTransferred,
              Date.now() - startTime
            );
            this.options.onProgress(speed);
          }
        );
        setTimeout(sendData, 0);
      }
    };

    sendData();
  }

  stop(): void {
    this.server.close();
  }
}

export class UdpClient extends SpeedTestBase {
  private client: dgram.Socket;

  constructor(
    private host: string,
    options: SpeedTestOptions
  ) {
    super({
      ...options,
      chunkSize: Math.min(
        options.chunkSize ?? MAX_UDP_PACKET_SIZE,
        MAX_UDP_PACKET_SIZE
      ),
    });
    this.client = dgram.createSocket("udp4");
  }

  async start(): Promise<SpeedTestResult> {
    return new Promise((resolve, reject) => {
      this.startTime = Date.now();

      this.client.on("error", (err) => {
        this.client.close();
        reject(err);
      });

      // Send start message with test type and chunk size
      const startMessage: TestMessage = {
        type: "start",
        testType: this.options.testType,
        chunkSize: this.options.chunkSize,
      };

      this.client.send(
        JSON.stringify(startMessage),
        this.options.port,
        this.host,
        (err) => {
          if (err) {
            reject(err);
            return;
          }

          if (this.options.testType === "upload") {
            this.startUpload();
          } else {
            // Set up receiver for download test
            this.client.on("message", (data) => {
              this.bytesTransferred += data.length;
              this.reportProgress();
            });
          }
        }
      );

      // Set up a timer to end the test
      setTimeout(() => {
        const result = this.getResult();
        this.stop();
        resolve(result);
      }, this.options.duration);
    });
  }

  private startUpload(): void {
    const chunk: Buffer = this.createChunk();

    const sendData = () => {
      if (Date.now() - this.startTime < this.options.duration) {
        this.client.send(
          new Uint8Array(chunk),
          this.options.port,
          this.host,
          (err) => {
            if (err) {
              console.error("Error sending data:", err);
              return;
            }
            this.bytesTransferred += chunk.length;
            this.reportProgress();
            setTimeout(sendData, 0);
          }
        );
      } else {
        this.client.close();
      }
    };

    sendData();
  }

  stop(): void {
    this.client.close();
  }
}
