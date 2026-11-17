# Gap Index — `tantra-ledger`

Per workspace rule `.agents/rules/gap-tracking.md`:
- **Gaps** (`docs/gaps/XXXX-*.md`): individual missing features, stubs, or rule violations.
- **Plans** (`docs/plans/XXXX-*.md`): collections of gaps.
- Numbering is per-repo, sequential, never reused.

Dismantled from the former `docs/roadmap/` directory: every completed milestone
now lives in [`docs/design/`](../design/architecture.md) +
[`docs/design/implementation-record.md`](../design/implementation-record.md);
the remaining future work is enumerated below.

## Open Gaps

| # | Title | Severity | Status | Plan |
|---|-------|----------|--------|------|
| _(none)_ | — | — | — | — |

## Closed Gaps

| # | Title | Closed In |
|---|-------|-----------|
| 0012 | No bracket-order (SL/TSL) OMS lifecycle — entry + SL/target child orders, TSL modify events, stop-trigger close (Elephant Edge EE+) | Closed 2026-11-17 (T12.7, `1e296d7`): `BracketOrder` aggregate + `BracketId` + `BracketEvent` (EntryFilled/StopMoved/PartialExit/StopTriggered/Closed) — `new` (stop-side validation), `on_entry_fill`, `apply_stop_update` (never-widen + qty ≤ open → `StopWidening`/`StopQtyExceedsOpen`), `on_partial_exit` (short-flip guard + FIFO target removal), `on_stop_trigger` (exact-Decimal realized PnL), `apply_fill_to_bracket` routing; `OmsSnapshot.open_brackets: Vec<BracketView>` (serde-defaulted); new `OrderError` variants. Tests `bracket_tests` (12) + `bracket_snapshot_tests` (4) + `bracket_proptest`; 55 passed. Follow-on: FIFO P&L realization via `portfolio.apply_fill` (oms crate has no portfolio dep yet). |
| 0001 | Allocation `Hrp` objective declared but not implemented | plan 0003 — HRP solver (correlation-distance single linkage, quasi-diagonalization, recursive bisection), `2026-11-13` |
| 0002 | Allocation `BlackLitterman` objective declared but not implemented | plan 0003 — `BlackLittermanViews`/`View` inputs + posterior blend, `2026-11-13` |
| 0003 | Allocation bound handling is clip-and-renormalize, not exact bound-constrained QP | plan 0003 — `project_box_simplex` + projected Sharpe / projected fixed point / HRP projection, `2026-11-13` |
| 0004 | Discrete / whole-share allocation not implemented | plan 0003 — `allocate_discrete` (iterative largest-remainder), `2026-11-13` |
| 0005 | Performance `MetricsInput::new()` mints `as_of` via `Utc::now()` | plan 0004 — `as_of` required constructor arg; runtime call sites updated, `2026-11-13` |
| 0006 | Performance edge score is a v1 placeholder formula | plan 0004 — v2 weighted spec (`EDGE_SCORE_VERSION = 2`), `2026-11-13` |
| 0007 | Portfolio `ClearingCalendar::new()` hardcodes NSE holidays 2024–2026 | plan 0005 — `new()` is an empty weekend-only calendar; real holidays injected via `with_holidays` (provider `tantra-data` gap 0015, consumer `tantra-runtime-backtest::NseTradingCalendar`), `2026-11-13` |
| 0008 | Corporate-action adjustments are aggregate-level, not per-lot | plan 0005 — per-lot `adjust_lot_for_*` + `AccountState::apply_corporate_actions_to_lots` (positions rebuilt as projections over the lot book), `2026-11-13` |
| 0009 | Journal exchange turnover fee tables fall back to NSE cash-segment rate for NFO/MCX/CDS | plan 0005 — per-segment Decimal tables in `tantra-market-finance::india::costs::segments`; `compute_charges` looks the segment rate up, `2026-11-13` |
| 0010 | Golden contract-note framework runs on the legacy floor-rounded fee track | plan 0005 — `GoldenTestRunner` switched onto `compute_charges`; fixtures re-based onto the canonical track (full pass at 1 paise), `2026-11-13` |
| 0011 | OMS context types (`OmsConfig`, `MarketData`, `PortfolioRiskState`) defined but not wired into validation | plan 0005 — `validate_order_against_context` + `submit_order_with_context` + maintained `OmsSnapshot`, `2026-11-13` |
| 0013 | Multi-Period Optimizer (MPO) not implemented | plan `tantra-agent` 0007 Track 4 Ph 11 2026-11-14 — `mpo::{multi_period_optimize, MpoConfig, MpoStep, MpoResult}` (per-period box-constrained solve + turnover cap + `tc·turnover² + penalty·turnover` friction; 6 tests), commit `126087f` |
