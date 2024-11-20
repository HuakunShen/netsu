import type {
  Protocol,
  SpeedTestClientOptions,
  SpeedTestServerOptions,
} from "./types";
import { TcpServer, TcpClient } from "./implementations/TcpSpeedTest";
import { UdpServer, UdpClient } from "./implementations/UdpSpeedTest";
import {
  WebSocketServer,
  WebSocketClient,
} from "./implementations/WebSocketSpeedTest";

export class SpeedTestFactory {
  static createServer(options: SpeedTestServerOptions) {
    switch (options.protocol) {
      case "tcp":
        return new TcpServer(options);
      case "udp":
        return new UdpServer(options);
      case "websocket":
        return new WebSocketServer(options);
      default:
        throw new Error(`Unsupported protocol: ${options.protocol}`);
    }
  }

  static createClient(host: string, options: SpeedTestClientOptions) {
    switch (options.protocol) {
      case "tcp":
        return new TcpClient(host, options);
      case "udp":
        return new UdpClient(host, options);
      case "websocket":
        return new WebSocketClient(host, options);
      default:
        throw new Error(`Unsupported protocol: ${options.protocol}`);
    }
  }
}
