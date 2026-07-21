# Protocol and Interoperability

**Updated: 2026-07-22**

`PROTOCOL.md` is the normative compatibility contract. It defines the iperf3
control-state lifecycle, 37-byte session cookie, framed JSON exchange, stream
setup, result exchange, and error behavior. TypeScript and Rust implementations
must derive the same stream identifiers and preserve that ordering to interoperate
with official iperf3.

## Supported compatibility matrix

| Transport | netsu TypeScript ↔ Rust | netsu ↔ official iperf3 |
| --- | --- | --- |
| TCP | Supported | Supported |
| UDP | Supported | Supported |
| WebSocket | Supported | Not supported — netsu extension |
| iroh/QUIC | Rust feature only | Not an iperf3 transport |

WebSocket binary frames act as a byte pipe: the cookie, control states,
length-prefixed JSON, and payload bytes are identical to TCP. Implementations
therefore reassemble arbitrary frame fragmentation before protocol reads.

## Conformance testing

`interop/run-matrix.ts`, run via `bun run e2e`, drives Docker containers for
each supported client/server/transport/direction combination. It is the proof
that independently implemented peers exchange data correctly; unit tests alone
cannot establish that boundary.

The matrix deliberately does not assert absolute throughput. Docker network
figures are not real-link performance measurements.

## Related pages

- [System Overview](../System%20Overview.md)
- [Rust Implementation](../Services/Rust%20Implementation.md)
