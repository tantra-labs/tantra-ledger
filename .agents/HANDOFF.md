# HANDOFF — tantra-ledger

**Status**: T2 track complete — plans 0003 (allocation phases 2–4) and 0004
(performance determinism & edge score v2) implemented; gaps 0001–0006 closed.
**Date**: 2026-11-13 · Repo: `tantra-ledger` · Branch: `main`

## Phase / Slice Status (COMPLETE)

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

- T2.7–T2.11 (plan 0005, settlement & fee consolidation — Proposed, not
  approved): external clearing-calendar source, per-lot corporate-action
  adjustments, per-segment fee tables, canonical golden framework, OMS context
  wiring. Do not start until the plan is approved.

## Operating Invariants

- Canonical profile `prod`: `cargo build/check/test --profile prod`.
- Tests live in `tests/` parallel to `src/`; no in-file `#[test]` blocks.
- Zero-I/O, zero floats (`rust_decimal::Decimal`), caller-injected timestamps —
  no `Utc::now()` in `crates/*/src/`.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets --profile prod -- -D warnings`,
  `cargo test --workspace --profile prod` green.
