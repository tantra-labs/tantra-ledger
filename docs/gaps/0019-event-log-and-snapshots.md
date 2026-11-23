# 0019 — Aggregate state is not serializable or replayable: no event log, no snapshots

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: deployment · **Severity**: high
**Plan**: [0006](../plans/0006-design-review-2026-11.md)

## What's stubbed or hardcoded

- `OrderStateMachine` (`crates/tantra-ledger-oms/src/lib.rs:290`, `#[derive(Debug, Clone)]` only), `OrderEntry` (no serde), `DoubleEntryLedger` (`crates/tantra-ledger-journal/src/lib.rs:150`, no serde), `AccountState`/`PortfolioState` (`crates/tantra-ledger-portfolio/src/lib.rs:875-893`, `Debug, Clone` only), `DematHoldings`, `ClearingCalendar`: none derive `Serialize/Deserialize`, while `TaxLot`, `Fill`, `CorporateAction`, `TradeManager`, `BracketOrder` do.
- No event log type: no `LedgerEvent`/`OrderEvent`, no `apply(event)` / `from_events(&[Event])`, no snapshot version tag.
- `crates/tantra-ledger/src/repository.rs:210-250` — repository traits are `&mut self` sync CRUD with only in-memory `HashMap` impls; the `ConcurrencyConflict` variant is declared and never produced.
- Runtime compensates ad hoc: `tantra-runtime/crates/tantra-runtime-actors/src/portfolio.rs` keeps `fills_history: Vec<(AccountState clone, ...)>` (clones the full account per fill).

## Why

`AGENTS.md` §5 requires restart-safe trading state. A pod restart loses every order, lot and ledger entry, and two replicas cannot share an account because state is not replayable from an ordered log. SEBI audit retention needs an exportable event stream (root gap 0023); deployment architecture is root plan 0022.

## How to close it

1. Define `OrderEvent`, `PortfolioEvent`, `LedgerEvent` (serde, `#[serde(tag = "type")]` plus a version) as the single source of truth.
2. Give each aggregate `apply(&mut self, &Event)`, `from_events`, `snapshot()`/`restore()` with `schema_version`.
3. Add `expected_version` to repository `save_*` for optimistic concurrency and produce `ConcurrencyConflict`.
4. Implement persistence in `tantra-runtime-persistence` against these traits (consumer work, not in this repo).

**Verification**: golden JSON event vectors in `tests/` for cross-version replay; proptest `from_events(events) == incrementally applied state`; snapshot/restore round trip. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `make check`, targeted `cargo test -p tantra-ledger-oms --profile prod`, `make prod`.

## Related

- [0016](0016-oms-typestate-transitions.md), [0018](0018-journal-invariants-and-chart-of-accounts.md), [0014](0014-corporate-action-scoping.md) (applied-action ids), [0022](0022-bounded-state-and-snapshots.md).
- Root plan 0022 (deployment architecture), root gap 0023 (tamper-evident audit trail). Root plan 0019 only lists unwired crates.
