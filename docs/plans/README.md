# Plans Index — `tantra-ledger`

Per workspace rule `.agents/rules/gap-tracking.md`:
- **Gaps** (`docs/gaps/XXXX-*.md`): individual missing features, stubs, or rule violations.
- **Plans** (`docs/plans/XXXX-*.md`): collections of gaps.
- When implemented, plan documents are cleaned up from `docs/plans/` and recorded in this index.
- Numbering is per-repo, sequential, never reused.

The former `docs/roadmap/` directory has been dismantled into this index plus
`docs/gaps/` and `docs/design/`: the completed Tier 1B migration milestones and
the subsequent allocation/performance landings live in
[`docs/design/architecture.md`](../design/architecture.md) and
[`docs/design/implementation-record.md`](../design/implementation-record.md);
every remaining roadmap item is now a numbered gap under
[`docs/gaps/`](../gaps/README.md), grouped into the active plans below.

## Active Plans

| # | Title | Target Scope | Status |
|---|-------|--------------|--------|
| *(None)* | — | — | All filed plans implemented |

## Implemented Plans

| # | Title | Target Scope | Implemented In |
|---|-------|--------------|----------------|
| 0001 | Portfolio Allocation & Optimization Engine | `tantra-ledger-allocation` | Phase 1: `AllocationObjective::{MeanVariance, RiskParity}`, `Constraint::WeightBounds`, `PortfolioState` read-adapter, `TargetWeights` (custom serde), pure `optimize()` in exact `Decimal` (Gaussian elimination, fixed-point risk parity, clip-and-renormalize bounds); 17 tests in `tests/optimize_tests.rs`; commit `b35a314`. Phases 2–4 shipped by plan 0003 (closed). |
| 0002 | Trade performance journal: annotations & analytics engine | `tantra-ledger-performance` | Phases 1–3: data model (`JournalAccount`, `TradeAnnotation`, `JournalNote`, `ClosedTrade`) + `JournalStore`; `PerformanceMetrics` engine + `EdgeScore` (v1 formula); `PnlCalendar` daily P&L aggregation + `compare_accounts` / `compare_account_vs_prior`. Pure, zero-I/O, zero-float; 42 tests in `tests/`; commits `2a3a04e`, `1b7fb97`. Determinism + v2 edge score shipped by plan 0004 (closed). |
| 0003 | Allocation Engine — Phases 2–4: Hrp, Black-Litterman, exact bounds, discrete allocation | `tantra-ledger-allocation` | `Hrp` (correlation-distance single linkage, quasi-diagonalization, recursive bisection) and `BlackLitterman` (`BlackLittermanViews`/`View`, reverse-optimized prior, posterior blend) objectives; exact bounded-simplex projection `project_box_simplex` replacing clip-and-renormalize (projected-gradient Sharpe for mean-variance, projected fixed point for risk parity); `allocate_discrete` largest-remainder whole-lot rounding. Gaps 0001–0004 closed; 32 tests in `tests/optimize_tests.rs`; commit `9f8ec22`. |
| 0004 | Performance Journal — temporal determinism & real edge score | `tantra-ledger-performance` | `MetricsInput::new(closed_trades, as_of)` — `as_of` required, the workspace's only `Utc::now()` in `crates/*/src/` removed (runtime `tantra-runtime-api-rest` call sites updated in lockstep); edge score v2 documented weighted blend (`EDGE_SCORE_VERSION = 2`, `EDGE_WEIGHT_*` consts, payoff-ratio bracket 0.5/1/2 → 0/50/100). Gaps 0005–0006 closed; golden vectors in `tests/metrics_tests.rs`; commit `9f8ec22`. |
| 0005 | Settlement, Corporate Actions & Statutory-Fee Consolidation | `tantra-ledger-portfolio`, `tantra-ledger-journal`, `tantra-ledger-oms` | Approved 2026-11-13; implemented 2026-11-13. **T2.7**: `ClearingCalendar::new()` is now an empty weekend-only calendar — the hardcoded NSE 2024–26 table is gone; real holidays are injected via `with_holidays` by `tantra-runtime-backtest::NseTradingCalendar` (data owner `tantra-data`, tracked by `tantra-data/docs/gaps/0015-*`). **T2.8**: per-lot corporate actions — `adjust_lot_for_split/bonus/demerger` + `reduce_lot_for_demerger` + `AccountState::apply_corporate_actions_to_lots` (positions rebuilt as projections over the lot book; FIFO `match_lots` uses adjusted basis). **T2.9**: per-segment (NFO/MCX/CDS) exchange turnover tables in `tantra-market-finance::india::costs::segments` (Decimal, from the researched circulars); `compute_charges` looks the segment rate up, NSE cash fallback removed; `stt_charge` updated to the Finance Act 2026 rates (futures 0.05%, options 0.15% of premium). **T2.10**: `GoldenTestRunner` switched onto `compute_charges`; sample fixtures re-based onto the canonical track (full pass at 1 paise). **T2.11**: OMS context canvas wired — `validate_order_against_context` (notional / configured qty / price collar / daily loss), `submit_order_with_context`, maintained `OmsSnapshot`. Gaps 0007–0011 closed. |
| 0006 | FIRE Planning Engine, 3-Bucket Glidepath Allocation & SWR Metrics | `tantra-ledger-allocation`, `tantra-ledger-performance` | **T19.1 (P0) Implemented 2026-11-18** (zero `tantra-core` edits; reuses `tantra_core::{Money, Currency}` & `SplitMix64` read-only). `tantra-ledger-allocation::fire` (`FirePlanConfig`, `TaxDragConfig::india_finance_act_2025`, `simulate_fire_runway`, `calculate_fire_archetypes`, `simulate_post_fire_cashflow`, `run_sor_stress` with 256-bin quantile table), `tantra-ledger-allocation::fire_buckets` (`FireBucketKind`, `FireGlidepathPolicy`, `compute_bucket_drift`, `allocate_fire_bucket_discrete`), and `tantra-ledger-performance::swr` (`SwrEvaluationInput`, `SwrReport`, `evaluate_swr`). 15 new tests (golden `tests/golden/fire_golden.json`, `proptest`, glidepath/drift/SWR) across `fire_engine_tests.rs`, `fire_buckets_tests.rs`, and `swr_tests.rs`. |
