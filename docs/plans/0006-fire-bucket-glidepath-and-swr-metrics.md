# 0006 — FIRE Planning Engine, 3-Bucket Glidepath Allocation & SWR Sustainability Metrics

**Status**: Proposed (2026-11-18) — Track **T19.1** (Phase **P0** — Primary L1 Domain Owner)

Master plan: root [`docs/plans/0013-fire-planning-and-cashflow-platform.md`](../../../docs/plans/0013-fire-planning-and-cashflow-platform.md).
Source inspiration: [`Elicherla01/fireapp`](https://github.com/Elicherla01/fireapp/tree/main) (`src/lib/fire-math.ts`).

---

## 0. Executive Summary & Objective

Own the canonical, zero-float, zero-I/O, zero-clock, `wasm32`-compatible FIRE planning domain types, simulation engine, 5-archetype retirement calculator, **3-Bucket Post-FIRE Cashflow & Glidepath** allocator, **Sequence-of-Returns Risk (SoRR)** bootstrapper, and **Safe Withdrawal Rate (SWR)** sustainability metrics inside **`tantra-ledger`** (`tantra-ledger-allocation` & `tantra-ledger-performance`).

**Architectural Decision**: `tantra-core` (L0) is **intentionally left untouched**. Personal financial independence planning, withdrawal schedules, and bucket glidepaths are portfolio allocation & performance concerns that belong in L1 (`tantra-ledger`), reusing `tantra_core::{Currency, Money}` and `tantra_core::analytics::distribution::SplitMix64` directly without expanding `tantra-core`.

---

## 1. `tantra-ledger-allocation` — FIRE Domain & Simulation Kernel (`src/fire.rs`)

All quantities, percentages (explicitly scaled percentage points or normalized ratios via helpers), multipliers, and currency values use `rust_decimal::Decimal` (`rust_decimal::MathematicalOps`) and `tantra_core::Money`. Zero `f32`/`f64` math, zero `tokio`/`std::fs`, and zero `Utc::now()`. Compiles cleanly for `wasm32-unknown-unknown`.

### 1.1 Wire & Value Types (re-exported cleanly; OpenAPI `schemars` adapters live in `tantra-runtime-api`)
- `ExpenseCategory`: `Essentials | Security | Discretionary`.
- `ExpenseItem`: `{ id: String, name: String, category: ExpenseCategory, monthly_amount: Money }`.
- `ExpenseInputMode`: `Itemized(Vec<ExpenseItem>) | QuickTotal(Money)`.
- `TaxDragConfig`: `{ stcg_rate_pct: Decimal, ltcg_rate_pct: Decimal, annual_ltcg_exemption: Money, realized_turnover_pct: Decimal }` (optional; default zero drag for gross model parity, plus explicit India Finance Act 2025 constructor: `20%` STCG, `12.5%` LTCG above ₹1,25,000).
- `FirePlanConfig`:
  - `currency: Currency` (default `Currency::Inr`)
  - `current_age: u16`, `coast_target_age: u16` (default `58`)
  - `expenses: ExpenseInputMode`
  - `multiplier: Decimal` (`20..=50`, default `30`)
  - `current_corpus: Money`
  - `monthly_savings: Money`
  - `yearly_step_up_pct: Decimal`
  - `expected_return_pct: Decimal`
  - `expected_inflation_pct: Decimal`
  - `tax_drag: Option<TaxDragConfig>`
  - `max_years: u16` (default `35`, bounded `1..=60`)
- `YearlyProjectionPoint`: `{ year: u16, age: u16, corpus_nominal: Money, corpus_real: Money, invested_capital_nominal: Money, total_gains_nominal: Money, monthly_sip: Money, fire_target_nominal: Money, fire_target_real: Money, is_fire_reached: bool }`.
- `FireCrossoverMilestone`: `{ years_to_fire: Option<Decimal>, months_to_fire: Option<u32>, fire_age: Option<Decimal>, fire_corpus_at_achieve: Option<Money>, target_nominal_at_achieve: Option<Money> }`.
- `FireArchetypeId`: `Lean | Standard | Fat | Coast | Barista`.
- `FireArchetypeCard`: `{ id: FireArchetypeId, multiplier: Decimal, swr_pct: Decimal, monthly_budget: Money, target_corpus: Money, shortfall: Money, progress_pct: Decimal, coast_years_remaining: Option<u16>, achieved_today: bool }`.
- `CashflowBucketConfig`: `{ equity_pct: Decimal, debt_pct: Decimal, theta_margin_pct: Decimal, equity_annual_yield_pct: Decimal, debt_annual_yield_pct: Decimal, theta_monthly_yield_pct: Decimal }` (validated $\sum \text{bucket\%} = 100\%$).
- `PostFireCashflowReport`: `{ monthly_equity_income: Money, monthly_debt_income: Money, monthly_theta_income: Money, total_passive_monthly: Money, monthly_expense_target: Money, surplus_or_deficit: Money, expense_coverage_pct: Decimal, annual_principal_drawdown_pct: Decimal }`.
- `SorStressConfig`: `{ seed: u64, post_fire_years: u16, annual_volatility_pct: Decimal, iterations: u32 }` (deterministic via `tantra_core::analytics::distribution::SplitMix64`, zero runtime clock).
- `SorStressResult`: `{ survival_probability_pct: Decimal, p10_terminal_real: Money, p50_terminal_real: Money, p90_terminal_real: Money, worst_depletion_year: Option<u16> }`.
- `FireSimulationReport`: `{ effective_monthly_expense: Money, effective_annual_expense: Money, base_fire_target_today: Money, swr_pct: Decimal, crossover: FireCrossoverMilestone, points: Vec<YearlyProjectionPoint>, archetypes: Vec<FireArchetypeCard>, post_fire_cashflow: Option<PostFireCashflowReport>, sor_stress: Option<SorStressResult> }`.

### 1.2 Pure Deterministic Functions
- `simulate_fire_runway(config: &FirePlanConfig) -> Result<FireSimulationReport, FireError>`: exact monthly discrete compounding with annual SIP step-up and intra-year crossover interpolation.
- `calculate_fire_archetypes(config: &FirePlanConfig) -> Result<Vec<FireArchetypeCard>, FireError>`: computes Lean (`0.75×` @ `22×`), Standard (`1.0×` @ config multiplier), Fat (`1.35×` @ `40×`), Coast (present corpus discounting target by $(1+r_{\text{real}})^N$), and Barista (`40%` covered via active consulting).
- `simulate_post_fire_cashflow(corpus: Money, monthly_expense: Money, mix: &CashflowBucketConfig) -> Result<PostFireCashflowReport, FireError>`.
- `run_sor_stress(config: &FirePlanConfig, initial_corpus: Money, sor: &SorStressConfig) -> Result<SorStressResult, FireError>`: pure seeded `SplitMix64` multi-year decumulation stress without modifying `tantra-core`.

---

## 2. `tantra-ledger-allocation` — FIRE 3-Bucket & Glidepath Allocation (`src/fire_buckets.rs`)

1. **Domain Types**:
   - `FireBucketKind`: `GrowthEquity | LiquidityRunway | ThetaMargin`.
   - `BucketClassifiedInstrument`: `{ instrument: InstrumentId, bucket: FireBucketKind }`.
   - `FireGlidepathPolicy`: interpolates target `(equity_pct, debt_pct, theta_margin_pct)` across years-to-FIRE (e.g., accumulation aggressive weight shifting smoothly toward the configured `CashflowBucketConfig` as `shortfall → 0`).
   - `FireBucketDrift`: compares live portfolio valuation per bucket against target `CashflowBucketConfig` percentages + discrete rupee/share deltas via `allocate_discrete`.
2. **Purity**: Exact `Decimal` box-simplex normalization; zero `f32`/`f64`; explicit `as_of: DateTime<Utc>` passed at the boundary.

---

## 3. `tantra-ledger-performance` — Withdrawal & Sustainability Metrics (`src/swr.rs`)

1. **Metrics**:
   - `SwrEvaluationInput`: `{ initial_corpus: Money, annual_withdrawal: Money, realized_period_returns: Vec<Decimal>, realized_income_yields: Vec<Money>, inflation_pct: Decimal, as_of: DateTime<Utc> }`.
   - `SwrReport`:
     - `effective_withdrawal_rate_pct: Decimal`
     - `passive_income_coverage_pct: Decimal` (ratio of dividends + arbitrage + options theta income to withdrawal need)
     - `real_capital_preservation_pct: Decimal`
     - `sequence_risk_drawdown_pct: Decimal` (early-year worst drawdown under withdrawal load)
     - `sustainable: bool`
2. **Integration**: Uses pure `Decimal` arithmetic only; zero clock queries.

---

## 4. Tests & Verification

- `crates/tantra-ledger-allocation/tests/fire_engine_tests.rs` & `fire_buckets_tests.rs`:
  - Parity tests matching all `fireapp` default preset scenarios & monthly step-up trajectories within 1 paisa (`Decimal` vs reference golden vectors exported in `tests/golden/fire_golden.json`).
  - Property-based tests (`proptest`) verifying monotonicity (`higher savings -> <= years_to_fire`), zero-inflation Nominal == Real identity, and invalid-input errors (`NegativeExpense`, `NegativeMultiplier`).
  - Glidepath bounds sum to `1.000000` (`project_box_simplex` invariant).
- `crates/tantra-ledger-performance/tests/swr_tests.rs`: sequence-risk stress scenarios, zero-passive-income edge case, explicit `as_of` determinism.
- Gate: `rg '\bf(32|64)\b' crates/*/src/`, `cargo fmt --check`, `cargo clippy --workspace --all-targets --profile prod -- -D warnings`, `cargo test --workspace --profile prod`, `cargo check -p tantra-ledger-allocation --target wasm32-unknown-unknown --profile prod`, `make prod`.
