# Multiplexing Lab

**Updated: 2026-07-22**

`netsu mux`, available with the Rust `iroh` feature, is a controlled
multiplexing and priority-latency lab. It tests whether high-priority, deadline
bound traffic remains responsive while bulk streams load one QUIC connection.
It is an experiment harness, not the speed-test CLI's compatibility path.

## Workloads and measurements

Built-in scenarios cover input-only, clipboard-only, file-only, input-plus-file,
and mixed traffic. Custom streams can specify a priority, rate or saturation,
frequency, and deadline. A probe stream records per-message round-trip latency
with an HDR histogram; bulk streams report throughput.

The lab supports local smoke runs, a listener/run pair sharing a code, and a
required-case matrix. Results can be written as schema-governed JSON and
per-message RTT NDJSON.

```sh
netsu mux local --scenario input-file --duration 10s
netsu mux run <code-or-ticket> --scenario mixed --priorities graded --json-out result.json
netsu mux matrix --duration 5s --output-dir out
```

Docker with `tc/netem` provides reproducible impairment experiments. Such runs
demonstrate behavior under the configured conditions; they do not substitute for
LAN or Internet performance evidence.

## Related pages

- [Rust Implementation](Rust%20Implementation.md)
- [Cross-device TUI](Cross-device%20TUI.md)
