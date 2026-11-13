# `tantra-ledger-allocation` — Portfolio Allocation Reference

Documents `crates/tantra-ledger-allocation/src/lib.rs`: pure, zero-I/O,
deterministic portfolio allocation producing `TargetWeights` (instrument →
target weight mappings). It never places an order — a caller (eventually
`tantra-exec` or a strategy layer) would turn the weights into orders. It
also exposes the discrete / whole-share allocator [`allocate_discrete`] that
maps continuous weights onto an executable lot book.

## Objectives

| Objective | Status | Description |
|-----------|--------|-------------|
| `AllocationObjective::MeanVariance` | Implemented | Markowitz tangency maximizing `(μᵀw − r_f) / √(wᵀΣw)` — solves `Σ·w = μ − r_f` by Gaussian elimination, normalizes; box constraints are enforced by projected-gradient Sharpe maximization over the bounded simplex. |
| `AllocationObjective::RiskParity` | Implemented | Equal risk contribution: `wᵢ(Σw)ᵢ` equal across instruments — damped fixed-point iteration (α = 0.5) with an exact bounded-simplex projection per iterate. |
| `AllocationObjective::Hrp` | Implemented | Hierarchical Risk Parity (López de Prado) — correlation-distance single-linkage dendrogram, quasi-diagonalization, recursive-bisection risk allocation, all in `Decimal`. |
| `AllocationObjective::BlackLitterman` | Implemented | View-blended posterior: `π = τ·Σ·w_mkt` prior, `μ_post = ((τΣ)⁻¹ + PᵀΩ⁻¹P)⁻¹·((τΣ)⁻¹π + PᵀΩ⁻¹q)`, posterior tangency normalized to sum 1. |

### Constraint

| Constraint | Purpose |
|------------|---------|
| `Constraint::WeightBounds { instrument, min, max }` | Box bound `min ≤ w ≤ max` per instrument (defaults `(0, 1)` long-only). |

Infeasible constraint sets (`min > max`, `sum(min) > 1`, `sum(max) < 1`),
singular covariance, missing return/covariance entries, and empty universes are
rejected with `AllocationError`.

## `PortfolioState` (read-adapter)

Builds optimization input from `tantra-ledger-portfolio`:

- `from_portfolio(portfolio, marks, as_of)` — universe = union of held
  positions; `current_weights` = market-value-normalized weights.
- `with_expected_returns()` / `with_covariance()` — attach estimates (from
  `tantra_core::quant` primitives, caller-provided).

## `TargetWeights` (output)

- `as_of`, `objective`, `weights: BTreeMap<InstrumentId, Decimal>` (sums to 1
  within `1e-9` tolerance).
- Custom JSON serialization: emitted as a sorted array of `{instrument, weight}`
  entries (an `InstrumentId` is a struct, not a valid JSON object key).
  `BlackLittermanViews` serializes its `market_weights` the same way.

## Zero-floats & determinism

All weights, bounds, returns, and covariance entries are exact
`rust_decimal::Decimal`. Matrix inversion (Gaussian elimination with partial
pivoting), risk-parity fixed point, hierarchical clustering, Black-Litterman
blending, and the bounded-simplex projection all run in `Decimal` (no
`f32`/`f64` even as intermediates). No network, filesystem, or clock queries;
`as_of` is injected explicitly by the caller.

## Bound handling (exact bounded-simplex projection)

`project_box_simplex` water-fills the box `{min ≤ w ≤ max, Σw = 1}` by binary
search on the single Lagrange parameter — the exact projection onto the
feasible set. The objectives enforce bounds with projected iterations
(mean-variance: projected-gradient Sharpe ascent with backtracking line
search; risk parity: projected fixed point; HRP: projection of the bisection
weights). This replaced the Phase 1 clip-and-renormalize heuristic, which did
not return the true constrained optimum for correlated universes.

## Discrete / whole-share allocation

`allocate_discrete(target, equity, prices, lot_sizes)` maps continuous
`TargetWeights` onto whole-lot `Quantity`s: floor each target notional to
whole lots, then spend residual cash one lot at a time on the largest
fractional remainders (iterative largest-remainder / Hamilton rounding,
deterministic in `Decimal`). The book never exceeds `equity`; the unspent
residual is documented to remain as cash.

## Related

- `tantra-ledger-portfolio` — `PortfolioState`, `Marks` (read-adapter source).
- `tantra-core::quant` — returns/covariance primitives (source for
  `expected_returns`/`covariance`).
- Tests: `crates/tantra-ledger-allocation/tests/optimize_tests.rs` (32).
