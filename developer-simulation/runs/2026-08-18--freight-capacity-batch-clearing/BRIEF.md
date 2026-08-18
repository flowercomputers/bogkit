# Trial 2: Deterministic batch clearing for a freight-capacity marketplace

## Developer role and exact Rust experience

You are a freight-marketplace backend developer with six years of production Ruby and PostgreSQL experience. You have used Rust for exactly five months, completed one instructor-led intermediate course, and built three internal batch utilities; you have never operated a Rust service in production and have not written unsafe Rust.

## Existing system and baseline

An existing marketplace lets shippers submit buy orders for pallet capacity and carriers submit sell offers for a lane and service date. PostgreSQL remains authoritative for orders, bookings, and payments. A Ruby batch worker clears an immutable snapshot every fifteen minutes and writes a proposed allocation for an operator to approve. Approval and booking occur in a separate existing system.

For this trial, a product-owner-signed price-time specification and a slow Ruby reference evaluator are authoritative. Each independent market is identified by `(lane_id, service_date)`. Buy orders rank by descending limit price, then ascending `submitted_sequence`, then ascending bytewise `order_id`. Sell offers rank by ascending ask price, then the same two tie breakers. While the best remaining buy price is at least the best remaining sell price, transfer the smaller remaining quantity at the seller's ask price. Stop when one side is empty or the best prices do not cross.

## Concrete pain

The Ruby worker inherits database arrival order when prices and timestamps tie, so a retry can choose different winners. It also materializes several copies of the batch, taking about nine minutes and more than 2.8 GiB on a large rehearsal. Operators need an advisory prototype that proves stable tie handling, exact capacity conservation, bounded resources, and all-or-nothing report publication before considering a worker rewrite.

## Workload and data shape

The benchmark snapshot contains exactly 650,000 orders across exactly 5,000 nonempty markets:

- 400,000 buy orders and 250,000 sell offers; `400,000 + 250,000 = 650,000`.
- Every market has at least one buy and one sell.
- Each row has `order_id`, `lane_id`, `service_date`, `side`, `submitted_sequence`, `quantity`, and `limit_price_cents`.
- IDs and lane IDs are nonempty ASCII. Order IDs are globally unique. Service dates use canonical `YYYY-MM-DD` text and are treated as labels, not local times.
- Quantity is an integer from 1 through 1,000,000 pallet units. Price is an integer from 1 through 10,000,000 cents per unit. Use checked arithmetic; aggregate value fields use `u128`.
- A fill records market key, monotonically increasing per-market fill index, buy ID, sell ID, integer quantity, and seller ask price.

One fill exhausts at least one currently active order. With both sides present, a market containing `B` buys and `S` sells can therefore emit at most `B + S - 1` fills. Across the fixture's 5,000 markets, the global cap is `400,000 + 250,000 - 5,000 = 645,000` fills. The program must reject a generated or hand-written result that exceeds this bound.

## Operational constraints

- Run offline on a declared two-core Linux machine with no network or database access.
- Treat the PostgreSQL snapshot export as immutable and advisory. Do not reserve capacity, create bookings, charge money, send notifications, or modify the source files.
- Ignore input row order. The declared ranking keys are the only priority source; do not use map iteration order, process timing, locale collation, or wall-clock time.
- Validate the full batch before clearing. Duplicate IDs, invalid dates, unknown sides, zero or out-of-range values, invalid ASCII fields, overlong lines, or inconsistent declared counts are run-level failures.
- Publish one complete canonical proposal or leave the previous complete proposal untouched, using a sibling temporary file and atomic same-filesystem rename.
- Use safe Rust only and no subprocesses. The batch must remain exactly reproducible without an external queue, cache, or service.

## Measurable acceptance criteria

### Correctness

1. Match the slow Ruby reference evaluator byte for byte on 10,000 generated small markets and all 40 hand-written golden markets, after canonical output ordering by market key and fill index.
2. For every fill, prove that both orders belong to the same market, the buy limit is at least the sell ask, the fill price equals the sell ask, and the quantity is positive and no greater than either order's remaining quantity.
3. For every order, the sum of filled quantity must be no greater than its offered quantity. For every market, total quantity filled on the buy side must equal total quantity filled on the sell side exactly.
4. After the final fill in a market, at least one side must be exhausted or the best remaining buy price must be below the best remaining sell price. No eligible higher-priority order may be bypassed.
5. On the main benchmark, emit no more than 645,000 fills. Recompute gross value as checked `u128` from `quantity * price_cents` and match the reference manifest exactly.
6. Cover partial fills, one order filling several counterparties, both orders exhausting together, noncrossing markets, equal prices, equal sequences resolved by ID, extreme accepted quantities and prices, and empty remainder on either side.

### Failure behavior

1. Pass all 80 supplied negative fixtures, including truncated NDJSON, invalid UTF-8, overlong lines, duplicate IDs, unknown sides, malformed dates, zero and excessive quantities, zero and excessive prices, malformed sequences, inconsistent manifest counts, forced read errors, forced write errors, and a deliberately over-cap result.
2. Every invalid batch must exit nonzero before publishing a proposal. If a prior proposal exists, its byte digest must remain unchanged.
3. Checked-arithmetic failure, allocation invariant failure, or reference-manifest mismatch must fail the whole run; no market may be published separately.
4. With injected exits after validation, after 1 percent and 50 percent of markets, and after temporary-file sync but before rename, rerunning must yield the canonical complete proposal and readers must never observe a partial file at the final path.

### Deterministic and runnable evidence

1. Provide `cargo test` as a single command that runs parsing, ordering, reference comparison, arithmetic, invariants, negative fixtures, and publication tests.
2. Provide a fixed-seed generator for the 650,000-order fixture and its manifest, plus one documented release-mode benchmark command. The manifest records market count, side counts, maximum fill count, and SHA-256 digests.
3. Shuffle all 650,000 input rows with each of ten fixed seeds. Every run must produce a byte-identical proposal. Repeat one shuffled input three times and show the same SHA-256 digest each time.
4. Run a property test over at least 10,000 generated small markets and compare every fill with the deliberately slow reference matcher. Store the seed on failure so the exact case can be rerun.
5. The benchmark command must emit machine-readable counts, elapsed time, peak resident memory from the supplied harness, scratch bytes, proposal digest, and pass/fail status. Evidence must run from a clean checkout with no supporting service.

### Performance and resources

1. Validate, clear, and publish the full 650,000-order snapshot in at most 20 seconds on the declared two-core machine.
2. Peak resident memory must not exceed 512 MiB. Scratch storage excluding the final proposal must not exceed 1 GiB, and the program may have at most 32 scratch files open at once.
3. The final proposal must not exceed 645,000 fill rows or 160 MiB, whichever is reached first. Exceeding either gate is a failure rather than permission to truncate.
4. On the same machine, the full run must finish in no more than one-third of the supplied Ruby worker's median time over three runs. Record all three baseline times and all three prototype times; use medians without extrapolation.

## Explicit non-goals

- Do not replace PostgreSQL, the approval workflow, booking, settlement, payment, notification, or carrier integrations.
- Do not optimize routes, predict prices, infer demand, rank participants by reputation, or add machine learning.
- Do not introduce partial fills smaller than the exact integer quantity rules, alternate auction pricing, hidden orders, all-or-none orders, cancellations during a batch, or cross-market substitution.
- Do not build a server, scheduler, queue consumer, distributed coordinator, user interface, or long-lived database.
- Do not publish the prototype into the production clearing path or act on its proposal.

## Compact self-contained prototype boundary

Build one safe-Rust CLI with a small testable library. It reads `orders.ndjson` and `manifest.json`, validates the entire immutable snapshot, groups orders by market, applies the exact declared matcher, checks conservation and the fill bound, and atomically writes canonical `proposal.ndjson`. Include a fixed-seed large-fixture generator, the deliberately slow reference matcher, 40 golden markets, 80 negative fixtures, property tests, a benchmark harness, and a short README containing exact test and benchmark commands. Stop at this advisory file-to-file boundary.
