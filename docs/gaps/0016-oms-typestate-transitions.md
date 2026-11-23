# 0016 — OMS lifecycle is runtime-checked only; mutators bypass `transition()`

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: high
**Plan**: [0006](../plans/0006-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-ledger-oms/src/lib.rs:322-345` — the legal graph is a `matches!` over `(OrderStatus, OrderStatus)`; `process_fill` (`:455-490`) and `cancel_order` assign `entry.state` directly (`process_fill` never calls `transition`).
- `crates/tantra-ledger-oms/src/lib.rs:253-262` — `OrderEntry` has all-`pub` fields; `register_order`/`register_request_ids` accept any `initial_state` (e.g. `Filled`) and any `filled_quantity` via struct literal.
- No public API for broker ack (`PendingNew -> New`), reject, expire, amend/replace or pending-cancel; the module doc (`crates/tantra-ledger-oms/src/lib.rs:17-20`) admits `PendingCancel` was dropped. The `(PendingNew, New)` / `(New, Rejected)` / `Expired` edges are unreachable except by direct field mutation.
- `Expired -> Expired` is the only Expired edge, and nothing produces `Expired`.

## Why

Cancel-vs-fill races cannot be represented: a cancel sent while a fill is in flight returns `OrderNotActive`, and the later genuine fill is silently rejected as `OrderNotActive`, so the OMS under-counts executed quantity versus the broker. Illegal states are constructible by any caller (`entry.state = ...`), against the DDD rule (`.agents/rules/coding-and-ddd.md` §1.2).

## How to close it

1. Make `OrderEntry` fields private with read accessors.
2. Introduce typestate or a single `apply(event: OrderEvent) -> Result<Transition, OrderError>` with `OrderEvent::{Ack, Reject, Fill, CancelRequested, CancelAcked, CancelRejected, Expire, ReplaceRequested/Acked}`; drive every state change through `transition()`.
3. Add `PendingCancel` to the OMS-local lifecycle (or a `cancel_requested: bool` flag) so late fills after a cancel request are accepted.
4. Record fills after terminal-cancel for in-flight quantity as an `Overfill`/`LateFill` event instead of dropping them.

**Verification**: proptest over random event interleavings checking `filled <= target`, terminal states absorbing, and reachable-state set equals the declared graph. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p tantra-ledger-oms --profile prod`, `make prod`.

## Related

- [0017](0017-fill-dedup-exec-id.md) (`process_fill` signature change), [0019](0019-event-log-and-snapshots.md) (`OrderEvent` is the same event type).
- Closed gap 0011 (OMS context wiring) and closed gap 0012 (bracket lifecycle) touch the same crate.
