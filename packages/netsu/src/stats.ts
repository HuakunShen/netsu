export function bitsPerSecond(bytes: number, seconds: number): number {
  return seconds > 0 ? (bytes * 8) / seconds : 0;
}

export interface IntervalReport {
  start: number; // seconds since test start
  end: number;
  bytes: number;
  bitsPerSecond: number;
}

/** Accumulates bytes; snap() closes the current interval and starts the next. */
export class IntervalMeter {
  #total = 0;
  #intervalBytes = 0;
  #startMs: number;
  #lastSnapMs: number;

  constructor(startMs: number) {
    this.#startMs = startMs;
    this.#lastSnapMs = startMs;
  }

  add(bytes: number): void {
    this.#total += bytes;
    this.#intervalBytes += bytes;
  }

  get totalBytes(): number {
    return this.#total;
  }

  snap(nowMs: number): IntervalReport {
    const seconds = (nowMs - this.#lastSnapMs) / 1000;
    const report: IntervalReport = {
      start: (this.#lastSnapMs - this.#startMs) / 1000,
      end: (nowMs - this.#startMs) / 1000,
      bytes: this.#intervalBytes,
      bitsPerSecond: bitsPerSecond(this.#intervalBytes, seconds),
    };
    this.#lastSnapMs = nowMs;
    this.#intervalBytes = 0;
    return report;
  }
}

/** RFC 1889 jitter + loss/reorder accounting for UDP receive side. */
export class JitterTracker {
  #jitterMs = 0;
  #prevTransit: number | undefined;
  #maxSeq = 0;
  #received = 0;
  #outOfOrder = 0;

  onPacket(pcount: number, sentMs: number, nowMs: number): void {
    this.#received++;
    if (pcount > this.#maxSeq) this.#maxSeq = pcount;
    else this.#outOfOrder++;

    const transit = nowMs - sentMs;
    if (this.#prevTransit !== undefined) {
      const d = Math.abs(transit - this.#prevTransit);
      this.#jitterMs += (d - this.#jitterMs) / 16;
    }
    this.#prevTransit = transit;
  }

  get jitterMs(): number {
    return this.#jitterMs;
  }
  get received(): number {
    return this.#received;
  }
  get maxSeq(): number {
    return this.#maxSeq;
  }
  get outOfOrder(): number {
    return this.#outOfOrder;
  }
  /** Expected (highest seq) minus received; late arrivals reduce loss. */
  get lost(): number {
    return Math.max(0, this.#maxSeq - this.#received);
  }
}
