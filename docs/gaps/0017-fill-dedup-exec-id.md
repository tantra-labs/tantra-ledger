# 0017 — No exec-id dedupe: duplicate or replayed fill reports are double-counted

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: high
**Plan**: [0006](../plans/0006-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-ledger-oms/src/lib.rs:455-490` — `process_fill(client_order_id, fill_quantity, now)` takes only quantity: no exec/trade id, no price, no `FillReport`, so there is nothing to dedupe on. A duplicated partial fill advances `filled_quantity` again (only an overshoot is rejected, via `FillExceedsTarget`).
- `crates/tantra-ledger-oms/src/lib.rs:253-262` — the entry tracks no `avg_fill_price`, no per-fill list, no commission; `snapshot()` (`crates/tantra-ledger-oms/src/lib.rs:~390`) hardcodes `total_commission/fees/today_pnl = 0` and `open_brackets: Vec::new()` ("until a real charge/PnL feed exists").
- Out-of-order reports (fill before ack) are tolerated only via `(PendingNew, PartiallyFilled)`, but the code never sets `New`.
- `crates/tantra-ledger/src/repository.rs:81` — `OrderRecord` separately carries `filled_quantity/avg_fill_price`: a second, richer order model that the OMS never feeds.

## Why

Broker WebSocket reconnects replay execution reports. A replayed partial fill of 40/100 marks the order 80/100 and later rejects the genuine last fill; position and ledger built from the same stream diverge from the broker. This is the ledger-side half of root gap 0010 (idempotency and fills without exec id).

## How to close it

1. Change to `process_fill(&FillReport)` keyed by `(client_order_id, exec_id)` with a bounded per-order `BTreeSet<ExecId>`; a replay returns idempotent `Ok(Duplicate)`.
2. Track `avg_fill_price` as an exact Decimal.
3. Unify `OrderEntry` and `repository::OrderRecord` (plan 0006 R1).
4. `OmsSnapshot` aggregates real commission/fees/P&L from fills, or the fields are dropped.

**Verification**: replay-duplicate proptest; golden broker exec-report sequences in `tests/`. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p tantra-ledger-oms --profile prod`, `make prod`.

## Related

- Root gap 0010 (end-to-end order idempotency); `tantra-core` plan 0006 R3/R5 (id typing); [0016](0016-oms-typestate-transitions.md), [0022](0022-bounded-state-and-snapshots.md) (bounded per-order exec-id sets).
