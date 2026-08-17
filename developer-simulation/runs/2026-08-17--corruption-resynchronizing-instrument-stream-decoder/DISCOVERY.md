# Discovery log

## Baseline-first hypothesis (recorded before component implementation inspection)

The public README and examples describe Fold as a durable incremental-view engine, ESE as static text embeddings, and ANNy as an approximate-nearest-neighbor index. The requested instrument decoder instead has a synchronous byte-in/records-out hot path with strict byte-retention, memory, latency, determinism, and privacy constraints.

The existing operational baseline is attractive because it is simple: append bytes, search for a terminator, then validate framing and CRC. Its predicted failure modes follow directly from that order of operations:

1. Waiting for a missing terminator can retain an unbounded prefix.
2. A false terminator inside opaque payload can make the parser test the wrong candidate.
3. Clearing the entire buffer after a corrupt candidate can discard the immediately following valid frame.
4. The absence of structured recovery diagnostics makes a parser-induced sequence gap indistinguishable from upstream loss.

The baseline should therefore remain a small, independent append-and-search implementation in the prototype, not be recast through BogKit. The candidate hypothesis is a bounded incremental decoder that searches for STX, rejects impossible declared lengths as soon as the three-byte prefix is present, waits for exactly `payload_length + 12` bytes, verifies ETX and CRC, and on rejection advances to the earliest later STX already present (or consumes enough bytes to make progress). It should retain at most 8,192 bytes per connection and emit only metadata plus a payload digest.

Initial component fit hypothesis, to be checked against the smallest public surfaces:

- **Fold:** possible fit only outside the consumption path for durable or aggregate diagnostics. It is not expected to help with framing, CRC, bounded resynchronization, or the in-memory per-connection state machine. Persistence in the decoder path would conflict with the no-disk-call constraint.
- **ESE:** no expected fit. Payloads are opaque bytes and no semantic text operation is requested.
- **ANNy:** no expected fit. Recovery is an exact byte-boundary problem, not approximate similarity search.

The smallest meaningful prototype should consequently use no BogKit component unless its public API reveals a direct bounded-stream primitive. That outcome would be a grounded no-fit result rather than a failure to use the library.

## Public-surface observations

1. The root README points a newcomer to `scripts/new-project.sh` but does not provide a decoder, byte-stream, bounded-buffer, or CRC example.
2. The starter and time-series examples show Fold owning persistent state and synchronously updating derived views in transactions.
3. The chat example explicitly places Fold behind a channel on a dedicated ingest thread, which reinforces that it need not sit in a network byte-consumption path.
4. The search example uses Fold, ESE, and ANNy for document indexing and semantic retrieval, a different problem shape from exact binary framing.

## Next validation

Inspect only the public crate surfaces needed to determine whether Fold has a non-persistent bounded stream primitive and whether ESE or ANNy expose anything relevant to exact byte framing. Then create an isolated child Rust workspace under `simulation-output/` and begin with failing behavior tests.

## Component-surface check

The narrow inspection confirmed the initial hypothesis:

1. `fold::stream::Stream` always opens a path-backed fjall database. Writes are transactional and commit to that store; the public stream surface is not a bounded in-memory byte decoder. Its useful conceptual lesson is the examples' separation of ingest ownership from readers, not a reusable framing primitive.
2. ESE's public operations accept text and produce fixed-size semantic embedding vectors. Instrument payload bytes may not be text and no semantic comparison belongs in framing.
3. ANNy's public surface is a mutable HNSW over fixed-dimensional vectors. Exact STX/length/CRC/ETX validation has no nearest-neighbor step.

Decision: use none of Fold, ESE, or ANNy in the hot-path prototype. Adding Fold only to count diagnostic classes would introduce disk I/O and a database lifecycle without testing the decoder's core risk. Such aggregation could be a downstream consumer in a production architecture, but it is outside this self-contained trial and its no-disk consumption-path rule.

## First review-driven correction (later rejected)

The initial candidate refinement—advance to a later STX only after the current declared candidate completes—was too conservative. The counterexample `02 00 13` followed by valid 14-byte frames for sequences 101 and 102 proved that a plausible in-range false header can suppress already complete, independently valid frames.

The round-one repair chose the earliest later STX whose own length, ETX, and CRC were fully proved while the earlier candidate remained incomplete. If the earlier candidate instead reached its declared end and failed, it preserved the earliest nested STX strictly inside those declared bytes. This passed the plausible-false-header follower case, but a second review found that it violated the protocol's opaque-payload contract.

## Second review-driven correction and incompatibility result

A clean frame for sequence 42 can contain a complete, CRC-valid encoded frame for sequence 777 at the start of its opaque payload. Whole-chunk delivery sees and emits the valid outer frame. The round-one one-byte policy emitted nested sequence 777 as soon as its ETX arrived and permanently lost the valid outer frame.

At the moment a nested-looking frame completes, bytes not yet received determine whether the containing accepted-length candidate will prove valid or fail. No parser can both treat arbitrary payload bytes as opaque and always emit a nested candidate immediately before that containing candidate ends. CRC proof for the nested bytes does not prove they are a transport boundary.

The responsible policy therefore never emits from inside an incomplete accepted-length candidate. It waits for the containing candidate to validate or fail; only after failure may it recover at the earliest nested STX. This restores chunk-independent clean framing, but the exact `02 00 13` plus 14-byte sequence-101 follower can no longer emit on that follower's ETX. The standalone prototype and the full brief are consequently `no_fit`. Fold, ESE, and ANNy still provide no primitive that can resolve missing information in the wire format.
