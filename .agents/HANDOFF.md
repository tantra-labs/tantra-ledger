# HANDOFF — tantra-ledger

**Status**: T2 track complete — plans 0003 (allocation phases 2–4), 0004
(performance determinism & edge score v2), and 0005 (settlement, corporate
actions & statutory-fee consolidation) implemented; gaps 0001–0011 closed.
**Date**: 2026-11-13 · Repo: `tantra-ledger` · Branch: `main`

## Phase / Slice Status (COMPLETE)

- **Plan 0005 — settlement, corporate actions & statutory fees** (T2.7–T2.11):
  - **T2.7** `ClearingCalendar::new()` is now an **empty weekend-only
    calendar** — the hardcoded NSE 2024–26 table (`add_nse_holidays_2024_2026`)
    is deleted. Real holidays are injected via `with_holidays` by
    `tantra-runtime-backtest::NseTradingCalendar` (the consumer & provider of
    `ClearingCalendar`); the holiday data owner is `tantra-data` (new gap
    `tantra-data/docs/gaps/0015-nse-trading-holiday-provider.md`).
  - **T2.8** per-lot corporate actions: `adjust_lot_for_split` /
    `adjust_lot_for_bonus` / `adjust_lot_for_demerger` /
    `reduce_lot_for_demerger` + `AccountState::apply_corporate_actions_to_lots`
    (positions rebuilt as projections over the lot book). FIFO `match_lots`
    realizes the correct per-lot PnL after a split.
  - **T2.9** per-segment (NFO/MCX/CDS) exchange turnover tables in
    `tantra-market-finance::india::costs::segments` (exact Decimal, from the
    researched circulars — see the plan appendix); `compute_charges` looks the
    segment rate up and the NSE cash-segment fallback is removed. `stt_charge`
    updated to the Finance Act 2026 rates (futures sell 0.05%, options sell
    0.15% of premium, effective 2026-04-01).
  - **T2.10** `GoldenTestRunner::compare_trade` switched onto `compute_charges`
    (canonical track); the five sample fixtures re-based onto the canonical
    track (real NSE turnover rate, options STT sell-side only, DP only on
    delivery sells); golden report passes at the 1-paise tolerance.
  - **T2.11** OMS context canvas wired: `validate_order_against_context`
    (notional / configured qty / price collar vs `MarketData` / daily loss vs
    `PortfolioRiskState`), `OrderStateMachine::submit_order_with_context`, and
    a maintained `snapshot()` feeding `OmsSnapshot`.
  - **Verification**: ledger workspace tests green (portfolio 47 incl. 10 new
    per-lot + calendar tests, journal 50 incl. 5 golden + per-segment tests,
    oms 38 incl. 13 new context tests), `cargo fmt --check` + `clippy
    --workspace --all-targets --profile prod -- -D warnings` clean, no
    `Utc::now()` in `crates/*/src/`, no production `src/` code calls the legacy
    fee track, no hardcoded holiday table in the domain crate. Runtime backtest
    + agent workspaces re-checked green. **API note**: `tantra-ledger-journal`
    gains a `tantra-market-finance` dependency (cross-repo path, declared in
    the ledger workspace dependencies); `stt_charge` futures/options rates
    changed per Finance Act 2026.

- **Plan 0003 — allocation phases 2–4** (`tantra-ledger-allocation`, commit
  `9f8ec22`):
  - **Hrp**: correlation-distance single-linkage dendrogram,
    quasi-diagonalization, recursive-bisection risk allocation — pure `Decimal`
    (`rust_decimal` `maths` feature added to the workspace dep).
  - **BlackLitterman**: `BlackLittermanViews` + `View` inputs (custom serde for
    `market_weights`), reverse-optimized prior `π = τ·Σ·w_mkt`, posterior blend
    `((τΣ)⁻¹ + PᵀΩ⁻¹P)⁻¹·((τΣ)⁻¹π + PᵀΩ⁻¹q)`, posterior tangency output. No views
    → market weights unchanged (golden).
  - **Exact bounds**: `project_box_simplex` water-fills the box
    `{min ≤ w ≤ max, Σw = 1}` (binary search on the Lagrange parameter),
    replacing clip-and-renormalize. MeanVariance maximizes Sharpe by projected
    gradient over the box (exact golden [0.625, 0.2, 0.175] vs the heuristic's
    [0.64, 0.2, 0.16]); RiskParity projects each fixed-point iterate.
  - **Discrete**: `allocate_discrete(target, equity, prices, lot_sizes)`
    iterative largest-remainder whole-lot rounding; residual stays as cash.
- **Plan 0004 — performance determinism & edge score v2**
  (`tantra-ledger-performance`, commit `9f8ec22`):
  - `MetricsInput::new(closed_trades, as_of)` — `as_of` now **required**; the
    only `Utc::now()` in the workspace's `crates/*/src/` is gone (runtime
    `tantra-runtime-api-rest::trade_journal` call sites updated in lockstep).
  - Edge score v2: `EDGE_SCORE_VERSION = 2`, documented `EDGE_WEIGHT_*` blend
    (PF 0.25 / WR 0.20 / DD 0.20 / AWL 0.15 / recovery 0.10 / consistency
    0.10), `score_payoff_ratio` bracket 0.5/1/2 → 0/50/100, consistency =
    0.6·day-win-rate + 0.4·streak balance. Golden vectors in
    `tests/metrics_tests.rs`.
- **Verification**: 200 workspace tests green (allocation 17 → 32, performance
  42 → 45), `cargo fmt --check` + `clippy --workspace --all-targets --profile
  prod -- -D warnings` clean, `grep -rn "Utc::now" crates/*/src/` returns
  nothing, no `#[cfg(test)]` in `crates/*/src/`. Agent MCP consumer
  (`tantra-agent-mcp` `hrp` tool) re-verified compiles.

## Decisions & Discoveries

- `project_box_simplex` bracketing: λ ∈ [minᵢ(xᵢ−maxᵢ), maxᵢ(xᵢ−minᵢ)] (the
  initial implementation had the interval flipped — fixed; the golden
  `mean_variance_exact_box_solver_golden` pins the corrected behavior).
- `score_payoff_ratio` replaces the v1 `lerp_score(0.5, 2)` whose doc claimed
  "1 → 50" but actually mapped 1 → 33.33; v2 bracket matches the doc.
- HRP on diagonal covariance ≡ inverse-variance weights (goldens for 2 and 3
  assets); HRP on a 2-asset universe is inverse-variance regardless of
  correlation.
- MCP consumer maps every `AllocationError` variant exhaustively — no new
  error variants were added.
- Clippy (rustc 1.95) newly flags two pre-existing `collapsible_if` in
  `performance/src/store.rs` and pre-existing fmt diffs in
  `ledger/src/repository.rs`, `oms/src/lib.rs`, `journal/src/lib.rs` — all
  fixed in the same change (Boy Scout).

## Immediate Next Step

- **Plan 0005 complete (T2.7–T2.11).** The remaining upstream ledger follow-on
  is `tantra-data/docs/gaps/0015-nse-trading-holiday-provider.md`: when
  `tantra-data` ships the real NSE trading-holiday ingestion, wire
  `tantra-runtime-backtest::NseTradingCalendar` to consume it (instead of its
  own seeded list). All T2 gaps 0007–0011 are closed; the ledger has no open
  gaps or plans.

## Operating Invariants

- Canonical profile `prod`: `cargo build/check/test --profile prod`.
- Tests live in `tests/` parallel to `src/`; no in-file `#[test]` blocks.
- Zero-I/O, zero floats (`rust_decimal::Decimal`), caller-injected timestamps —
  no `Utc::now()` in `crates/*/src/`.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets --profile prod -- -D warnings`,
  `cargo test --workspace --profile prod` green.
