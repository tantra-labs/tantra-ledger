# 0006 — tantra-ledger design review (2026-11): OMS/journal correctness, idempotent fills & event-sourced persistence

**Status**: Proposed (2026-11-23)
**Scope**: `tantra-ledger` · **Umbrella**: root `docs/plans/0021-workspace-design-review-2026-11.md`

## 0. Executive Summary & Objective

The 2026-11 review found that the ledger's pure-domain discipline is good (no wall clock, no I/O, no floats in money paths) but its aggregates do not yet enforce their own invariants. Corporate actions hit every instrument's lots and are not idempotent (critical); `apply_fill` ignores the fill side, which breaks the live `PortfolioActor`; the OMS lifecycle and the double-entry journal are guarded only at runtime and expose public mutable state; fills carry no exec id, so replays double-count; and no aggregate is serializable or replayable, which blocks restart-safe and multi-replica operation.

This plan makes the aggregates own their invariants (Phase A/B), then introduces an event-sourced state contract and bounded state (Phase C). Non-goals: cost-table moves and `String` errors in portfolio already owned by root `docs/plans/0019-codebase-audit-refactoring-unification-and-wiring.md` (P3.3, P4.1); persistence implementation lives in `tantra-runtime-persistence`.

## 1. Context & Gaps Covered

| Gap | Title | Kind | Severity |
|---|---|---|---|
| [0014](../gaps/0014-corporate-action-scoping.md) | Corporate actions not scoped to an instrument, non-idempotent | design-flaw | critical |
| [0015](../gaps/0015-apply-fill-ignores-side.md) | `apply_fill` ignores fill side | design-flaw | high |
| [0016](../gaps/0016-oms-typestate-transitions.md) | OMS lifecycle runtime-checked only | design-flaw | high |
| [0017](../gaps/0017-fill-dedup-exec-id.md) | No exec-id dedupe on fills | design-flaw | high |
| [0018](../gaps/0018-journal-invariants-and-chart-of-accounts.md) | Journal invariants and chart of accounts | design-flaw | high |
| [0019](../gaps/0019-event-log-and-snapshots.md) | No event log / snapshots; state not serializable | deployment | high |
| [0020](../gaps/0020-tax-lot-classification-and-dates.md) | Lot product classification and IST dates | design-flaw | medium |
| [0021](../gaps/0021-corporate-action-rounding.md) | Fractional-share handling in corporate actions | design-flaw | medium |
| [0022](../gaps/0022-bounded-state-and-snapshots.md) | Unbounded maps, no eviction/rollover | design-flaw | medium |

Plan-only items (no standalone gap doc):
- **R1 — Duplicate models inside the facade crate** — `OrderRecord` vs `OrderEntry` (`crates/tantra-ledger/src/repository.rs:81-100` vs `crates/tantra-ledger-oms/src/lib.rs:253`), `JournalTransaction` vs `Transaction` (`repository.rs:125-175` vs `crates/tantra-ledger-journal/src/lib.rs:125`), `PositionRecord` (`repository.rs:105-120`, hardcodes `unrealized_pnl: Money::inr(0)` in `project`, `:229-250`); string-payload `DomainError` (`repository.rs:34-52`) and `AccountRecord.status: String` (`:112`); `InMemoryLedgerRepository::post_entry` (`:~410`) is a third balance implementation. The `tantra-ledger` crate should be a thin facade per the facade convention.
- **R2 — Cross-repo position model sprawl** (root gap 0014) — five-plus position/lot implementations and three product enums: `HeldPosition`/`TaxLot` (`crates/tantra-ledger-portfolio/src/lib.rs:450`), `Product` (`:152`), `ProductType` (`crates/tantra-ledger-journal/src/contract_notes.rs:48`), `tantra-core/src/portfolio.rs:39`, `tantra-runtime/crates/tantra-runtime-api/src/wire.rs:756`, `tantra-exec/crates/tantra-exec-paper/src/paper.rs:55`, `tantra-exec/crates/tantra-exec-core/src/types.rs:22`, `tantra-ffi/python/tantra/position.py:32-110` (different average-price rounding, `quantize(0.01, ROUND_HALF_UP)`).
- **R3 — Settlement/demat API hygiene** (root gap 0020) — `Result<_, String>` and `u64`-quantity `apply_fill*`/`compute_capital_gains` (`crates/tantra-ledger-portfolio/src/lib.rs:1225, 1281, 1537, 1586`); T+1 settle date from the UTC date (`:353`); `pass_rate() -> f64` (`crates/tantra-ledger-journal/src/contract_notes.rs:340-344`); `unwrap/expect` in `src/` (`crates/tantra-ledger-portfolio/src/lib.rs:537`, `crates/tantra-ledger-performance/src/calendar.rs:68-120`, `crates/tantra-ledger-allocation/src/lib.rs:515, 1000`).
- **R4 — Two parallel tax calculators and hard-coded statutory rates** — legacy floor-rounded `TaxCalculator::stt/GST/stamp_duty` (`crates/tantra-ledger-journal/src/lib.rs:480-520`) vs canonical `round_dp(2)` `compute_charges` (`:560-739`); rates are literals with no effective-date table although STT changed 2026-04-01 (`:605`); `brokerage` (`:573`) bakes in one broker plan.

## 2. Component Specifications & Phases

### Phase A — Correctness fixes (small, test-first)
1. **0014** — instrument-scoped, applied-set idempotent corporate actions (wire version bump).
2. **0015** — `AccountState::apply_fill` owns open/add/reduce/reverse; coordinate removal of the backtest workaround and runtime call (`tantra-runtime`).
3. **0021** — rounding policy and total-basis storage (after 0014, same functions).
4. **R3** — `Quantity`-typed signatures, IST trade date, `pass_rate -> Decimal`, remove `expect`s.
**Gate**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `make check`, `cargo test -p tantra-ledger-portfolio --profile prod`, `make prod`.

### Phase B — Aggregate invariants & de-duplication
1. **0016** — private `OrderEntry`, `OrderEvent` apply, `PendingCancel`/late-fill handling.
2. **0017** — `process_fill(&FillReport)` with bounded exec-id set and `avg_fill_price`.
3. **0018** — `PostedTransaction`, private ledger fields, unique tx id, `AccountKind`.
4. **0020** — product-aware lots, IST `trade_date`, `MatchResult`, remove the sentinel date.
5. **R1** — repository traits over domain types; delete duplicate records; typed `DomainError`.
6. **R4** — `ChargeSchedule { effective_from, rates }` and `BrokeragePlan`; remove legacy floor methods after fixtures are re-based.
**Gate**: as Phase A plus `cargo test -p tantra-ledger-oms --profile prod`, `cargo test -p tantra-ledger-journal --profile prod`, shared contract suite against every `LedgerRepository` impl.

### Phase C — Event-sourced state & deployment readiness
1. **0019** — `OrderEvent`/`PortfolioEvent`/`LedgerEvent` with `apply`/`from_events`/`snapshot`/`restore`, `expected_version`, golden JSON event vectors.
2. **0022** — incremental counters, `archive_terminal`, `compact_exhausted_lots`, rollover contract.
3. **R2** — canonical `ProductType`/`Fill` in `tantra-core`; ledger `AccountState` as the only mutation engine, exposed to Python via `tantra-ffi`; Python `Position` becomes a read-only projection; shared cross-language golden vectors in `tantra-ffi/tests` and `tantra-ledger/tests`.
**Gate**: `make test`, `make prod`; wasm check for compute crates (`cargo check -p tantra-ledger-portfolio --target wasm32-unknown-unknown`).

Hygiene to fold in: drop `v4` from the workspace `uuid` features (domain must not mint UUIDs); move embedded fixtures and `generate_sample_csv` out of `contract_notes.rs` `src/` into `tests/fixtures`; check `thiserror = "1"` against 2.x in combined builds; `tantra-ledger-journal` depends on `tantra-market-finance` (L2, root plan 0019 P4.1), which blocks wasm32 for dependents.

## 3. Relationship to Other Plans

- Not duplicated here: root plan 0019 P3.1/P3.3 (portfolio structure, `String` errors) and P4.1 (cost tables move into journal); R4 is a follow-on to P4.1.
- Feeds root plan 0022 (restart-safe state, event stream for audit) via 0019/0022, and root gap 0023 (SEBI audit export).
- Related root gaps: 0010 (idempotency, via 0017), 0014 (R2), 0019 (0014/0021), 0020 (0020/R3).
- Consumer work in `tantra-runtime` (`PortfolioActor` hard-coded `"default"` account at `tantra-runtime/crates/tantra-runtime-actors/src/portfolio.rs:134`, backtest `engine.rs:105`) and `tantra-runtime-persistence`; depends on 0015/0019.

## 4. Exit Criteria

1. Gaps 0014-0022 closed and moved to the closed table; R1-R4 closed or re-filed as gaps.
2. A two-instrument corporate-action proptest and a double-apply test pass.
3. Backtest and live paths share one `AccountState::apply_fill`; the workaround is deleted.
4. No public mutable fields on `OrderEntry`, `DoubleEntryLedger`, `Transaction`.
5. Replaying a duplicate exec id is a no-op (proptest).
6. `from_events(events)` reproduces aggregate state; golden event vectors committed.
7. `make check`, `make test`, `make prod` green; no `#[cfg(test)]` in `src/`.
