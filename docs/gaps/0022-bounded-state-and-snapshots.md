# 0022 — Unbounded per-process maps; no eviction or end-of-day rollover

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: medium
**Plan**: [0006](../plans/0006-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-ledger-oms/src/lib.rs:290-300` — `orders: BTreeMap<ClientOrderId, OrderEntry>` retains terminal orders forever (the idempotency set is the order table); `all_order_ids()` clones the whole key set; `snapshot()` is O(n) per call and filters the full map twice (`:~390`).
- `crates/tantra-ledger-journal/src/lib.rs:150-154` — `transactions: Vec<Transaction>` grows forever; `AccountState.lots` (`crates/tantra-ledger-portfolio/src/lib.rs:893`) never compacts exhausted lots (they are only filtered at projection, `crates/tantra-ledger/src/repository.rs:229`).
- `crates/tantra-ledger-performance/src/store.rs:31-37` — four `HashMap`s with no cap.

## Why

A long-running algo grows without bound (`.agents/rules/scalability-and-concurrency.md` §3, bounded memory), and `snapshot()` on the hot path degrades linearly with order count.

## How to close it

1. Maintain OMS snapshot counters incrementally in the state machine instead of recomputing.
2. Add `archive_terminal(before: DateTime<Utc>) -> Vec<OrderEntry>` returning evicted entries for the persistence layer, while keeping a bounded recent-id ring/Bloom filter for idempotency.
3. Add `AccountState::compact_exhausted_lots()`.
4. Cap or evict the `performance::store` maps.
5. Document the daily rollover contract (what is archived, what idempotency window is retained).

**Verification**: proptest "incremental counters == recomputed counters"; eviction test showing idempotency holds for the recent window. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p tantra-ledger-oms --profile prod`, `make prod`.

## Related

- [0019](0019-event-log-and-snapshots.md) (archived entries flow to the event log), [0017](0017-fill-dedup-exec-id.md) (bounded per-order exec-id sets); root gap 0020 (day rollover).
