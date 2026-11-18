//! # व्यापार・आवंटन tantra-ledger-allocation
//!
//! Pure portfolio **allocation** — the decision of *what target weights to
//! hold* — as a distinct sibling of `tantra-ledger-portfolio`, which only
//! values and tracks what is already held (FIFO tax lots, marks, T+1
//! settlement). This crate is deliberately pure, zero-I/O and deterministic:
//! no network, no filesystem, no clock queries; every time-dependent input
//! arrives as an explicit `as_of` timestamp.
//!
//! It produces [`TargetWeights`] — a mapping from [`InstrumentId`] to a target
//! portfolio weight — that a caller (eventually `tantra-exec` or a strategy
//! layer, out of scope here) would turn into orders. It never places an order
//! itself.
//!
//! # Phase 1–4 scope
//!
//! [`AllocationObjective::MeanVariance`] and [`AllocationObjective::RiskParity`]
//! with simple per-instrument [`Constraint::WeightBounds`] only (no sector
//! limits, no turnover penalty). Phases 2–4 (plan 0003) add the declared
//! objectives: [`AllocationObjective::Hrp`] (hierarchical risk parity) and
//! [`AllocationObjective::BlackLitterman`] (view-blended posterior), an exact
//! box-constrained solve that replaces the Phase 1 clip-and-renormalize
//! heuristic, and the discrete / whole-share allocator
//! [`allocate_discrete`].
//!
//! # Multi-period optimization (plan 0007 Track 4 Phase 11 — gap 0013)
//!
//! [`multi_period_optimize`] sequences a rebalance over `T` periods under an
//! optional per-period L1 turnover cap and quadratic transaction costs —
//! the domain foundation for the future MCP gateway tool
//! `tantra_multi_period_optimize`. See [`mpo`] for the algorithm.
//!
//! # Zero-floats discipline
//!
//! All weights, bounds, returns, and covariance entries are exact
//! `rust_decimal::Decimal`. The numerics — Gaussian-elimination matrix
//! inversion, fixed-point risk parity, projected-gradient Sharpe
//! maximization, hierarchical clustering, and Black-Litterman blending — are
//! carried out entirely in `Decimal`, so no `f32`/`f64` enters this crate
//! even as an intermediate.
//!
//! # Bound handling
//!
//! Bound-constrained allocation is solved by **exact projection onto the
//! bounded simplex**: [`project_box_simplex`] water-fills the box
//! `{min ≤ w ≤ max, Σw = 1}` (binary search on the single Lagrange
//! parameter), and the objectives are run as projected iterations:
//! mean-variance maximizes the Sharpe ratio by projected gradient ascent,
//! risk parity is a projected fixed point, and HRP projects its bisection
//! weights. This replaces the Phase 1 clip-and-renormalize heuristic, which
//! did not return the true constrained optimum for correlated universes.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

pub use tantra_core::{InstrumentId, Money, Price, Quantity};

pub mod mpo;

pub use mpo::{MpoConfig, MpoResult, MpoStep, multi_period_optimize};

/// Convergence tolerance for the numerical loops (risk parity fixed point,
/// clip-and-renormalize): `1e-9`.
const EPS: Decimal = Decimal::from_parts(1, 0, 0, false, 9); // 0.000000001

/// The optimization objective.
///
/// All four planned objectives are declared now; `MeanVariance` and
/// `RiskParity` landed in Phase 1, `Hrp` and `BlackLitterman` in plan 0003
/// (Phases 2–3). Additional variants are additive future work rather than a
/// redesign of the enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AllocationObjective {
    /// Markowitz tangency portfolio maximizing `(μᵀw − r_f) / √(wᵀΣw)`.
    MeanVariance {
        /// The risk-free rate subtracted from expected returns.
        risk_free_rate: Decimal,
    },
    /// Equal risk contribution: `wᵢ(Σw)ᵢ` equal across all instruments.
    RiskParity,
    /// Hierarchical Risk Parity (López de Prado) — Phase 2.
    Hrp,
    /// Black-Litterman blended allocation (view-posterior mean) — Phase 3.
    BlackLitterman {
        /// The reverse-optimization and view-posterior inputs.
        views: BlackLittermanViews,
    },
}

/// A single investor view: a linear combination of instrument returns with an
/// expected level and a confidence (standard deviation of the view error).
///
/// The view is `q = P·r + ε` with `ε ~ N(0, uncertainty²)`; `P` is the
/// pick-vector row built from [`View::instruments`] × [`View::coefficients`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    /// Instruments with non-zero exposure in this view (the pick-vector
    /// columns; must all be in the universe).
    pub instruments: Vec<InstrumentId>,
    /// Coefficient per instrument (the pick-vector row entries).
    pub coefficients: Vec<Decimal>,
    /// The view's expected value `q`.
    pub mean: Decimal,
    /// Confidence: standard deviation of the view error (must be > 0).
    pub uncertainty: Decimal,
}

/// Black-Litterman inputs: the prior-scaling constant `τ`, the benchmark
/// (market-capitalization) weights reverse-optimized into the prior, and the
/// investor views.
///
/// JSON serialization mirrors [`TargetWeights`]: an `InstrumentId` is a struct,
/// so the `InstrumentId → Decimal` weight map is emitted as a sorted array of
/// `{ instrument, weight }` entries instead of a JSON object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlackLittermanViews {
    /// Prior scale `τ` (typically 0.01–0.10; must be > 0).
    pub tau: Decimal,
    /// Benchmark weight per instrument (must cover every universe instrument;
    /// normalized internally to sum to 1).
    pub market_weights: BTreeMap<InstrumentId, Decimal>,
    /// The investor views (`P`, `q`, `Ω`); may be empty (no views → the
    /// allocation reduces to the market weights).
    pub views: Vec<View>,
}

impl serde::Serialize for BlackLittermanViews {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("BlackLittermanViews", 3)?;
        state.serialize_field("tau", &self.tau)?;
        let entries: Vec<WeightEntry> = self
            .market_weights
            .iter()
            .map(|(instrument, weight)| WeightEntry {
                instrument: instrument.clone(),
                weight: *weight,
            })
            .collect();
        state.serialize_field("market_weights", &entries)?;
        state.serialize_field("views", &self.views)?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for BlackLittermanViews {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct BlackLittermanViewsWire {
            tau: Decimal,
            market_weights: Vec<WeightEntry>,
            views: Vec<View>,
        }
        let wire = BlackLittermanViewsWire::deserialize(deserializer)?;
        let market_weights = wire
            .market_weights
            .into_iter()
            .map(|entry| (entry.instrument, entry.weight))
            .collect();
        Ok(BlackLittermanViews {
            tau: wire.tau,
            market_weights,
            views: wire.views,
        })
    }
}

/// A constraint on the optimization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Constraint {
    /// Box bound on a single instrument's target weight: `min ≤ w ≤ max`.
    WeightBounds {
        /// The constrained instrument.
        instrument: InstrumentId,
        /// Minimum weight (0 = long-only; negative allows a short).
        min: Decimal,
        /// Maximum weight.
        max: Decimal,
    },
}

/// The optimization input: the instrument universe, the current book as
/// weights, and the return/covariance estimates the optimizer works from.
///
/// `expected_returns` and `covariance` come from return history (e.g. the
/// [`tantra_core::quant`] primitives); `current_weights` / the instrument
/// universe come from the caller's held positions via
/// [`PortfolioState::from_portfolio`] (the read-adapter over
/// `tantra-ledger-portfolio`).
#[derive(Debug, Clone, PartialEq)]
pub struct PortfolioState {
    /// As-of timestamp injected by the caller (temporal determinism).
    pub as_of: DateTime<Utc>,
    /// The ordered instrument universe (index `i` ↔ position `i` in every
    /// vector-shaped input below).
    pub instruments: Vec<InstrumentId>,
    /// Current portfolio weights, used as the optimization starting point.
    pub current_weights: BTreeMap<InstrumentId, Decimal>,
    /// Expected (annualized) per-instrument return, used by `MeanVariance`.
    pub expected_returns: BTreeMap<InstrumentId, Decimal>,
    /// The covariance matrix, `covariance[a][b]`, indexed by instrument.
    pub covariance: BTreeMap<InstrumentId, BTreeMap<InstrumentId, Decimal>>,
}

impl PortfolioState {
    /// Reads the optimization input from a `tantra-ledger-portfolio` state
    /// and its marks: the ordered universe is the union of every held
    /// position across all accounts, and `current_weights` are each position's
    /// market value (`marks × quantity`) normalized to the portfolio total.
    ///
    /// `expected_returns` and `covariance` cannot come from the portfolio
    /// crate (it tracks positions, not return history) and must be attached
    /// with [`PortfolioState::with_expected_returns`] /
    /// [`PortfolioState::with_covariance`].
    pub fn from_portfolio(
        portfolio: &tantra_ledger_portfolio::PortfolioState,
        marks: &tantra_ledger_portfolio::Marks,
        as_of: DateTime<Utc>,
    ) -> Self {
        let mut values: BTreeMap<InstrumentId, Decimal> = BTreeMap::new();
        for account in portfolio.accounts.values() {
            for pos in &account.positions {
                let market = marks
                    .marks
                    .get(&pos.instrument)
                    .copied()
                    .unwrap_or(Price::ZERO);
                let value = market.value() * pos.quantity.value();
                *values.entry(pos.instrument.clone()).or_default() += value;
            }
        }
        let total: Decimal = values.values().copied().sum();
        let instruments = values.keys().cloned().collect::<Vec<_>>();
        let current_weights = instruments
            .iter()
            .map(|id| {
                let v = values[id];
                let w = if total.is_zero() {
                    Decimal::ZERO
                } else {
                    v / total
                };
                (id.clone(), w)
            })
            .collect();
        Self {
            as_of,
            instruments,
            current_weights,
            expected_returns: BTreeMap::new(),
            covariance: BTreeMap::new(),
        }
    }

    /// Attaches the expected-return estimates (required for `MeanVariance`).
    pub fn with_expected_returns(
        mut self,
        expected_returns: BTreeMap<InstrumentId, Decimal>,
    ) -> Self {
        self.expected_returns = expected_returns;
        self
    }

    /// Attaches the covariance matrix.
    pub fn with_covariance(
        mut self,
        covariance: BTreeMap<InstrumentId, BTreeMap<InstrumentId, Decimal>>,
    ) -> Self {
        self.covariance = covariance;
        self
    }
}

/// The auditable output of an allocation run.
///
/// JSON serialization is custom: an `InstrumentId` is a struct, so the
/// `InstrumentId → Decimal` weight map cannot use JSON object keys. It is
/// emitted as a sorted array of `{ instrument, weight }` entries instead.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetWeights {
    /// As-of timestamp of the allocation.
    pub as_of: DateTime<Utc>,
    /// The objective that produced these weights.
    pub objective: AllocationObjective,
    /// Target weight per instrument, summing to 1 (within decimal tolerance).
    pub weights: BTreeMap<InstrumentId, Decimal>,
}

/// One serialized weight entry (see [`TargetWeights`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WeightEntry {
    instrument: InstrumentId,
    weight: Decimal,
}

impl serde::Serialize for TargetWeights {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("TargetWeights", 3)?;
        state.serialize_field("as_of", &self.as_of)?;
        state.serialize_field("objective", &self.objective)?;
        let entries: Vec<WeightEntry> = self
            .weights
            .iter()
            .map(|(instrument, weight)| WeightEntry {
                instrument: instrument.clone(),
                weight: *weight,
            })
            .collect();
        state.serialize_field("weights", &entries)?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for TargetWeights {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TargetWeightsWire {
            as_of: DateTime<Utc>,
            objective: AllocationObjective,
            weights: Vec<WeightEntry>,
        }
        let wire = TargetWeightsWire::deserialize(deserializer)?;
        let weights = wire
            .weights
            .into_iter()
            .map(|entry| (entry.instrument, entry.weight))
            .collect();
        Ok(TargetWeights {
            as_of: wire.as_of,
            objective: wire.objective,
            weights,
        })
    }
}

/// Allocation errors.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AllocationError {
    /// The constraint set cannot be satisfied (e.g. `min > max`, or the
    /// mandatory weight bounds sum outside `[0, 1]`).
    #[error("infeasible constraint set: {0}")]
    InfeasibleConstraints(String),

    /// The covariance matrix is singular (or not positive-definite) and the
    /// required inverse / solver step cannot be completed.
    #[error("covariance matrix is singular or not positive-definite")]
    SingularCovariance,

    /// Not enough return history was supplied to estimate the inputs.
    #[error("insufficient history: needed at least {needed} observations, got {got}")]
    InsufficientHistory {
        /// Observations required.
        needed: usize,
        /// Observations available.
        got: usize,
    },

    /// The objective is declared but not implemented in this phase.
    #[error("objective not implemented: {0}")]
    NotImplemented(String),

    /// The input is malformed (missing covariance/return entries, unknown
    /// instruments in constraints, empty universe).
    #[error("invalid allocation input: {0}")]
    InvalidArgument(String),
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Solves the allocation and returns auditable target weights.
///
/// Pure, zero-I/O and deterministic: identical inputs produce identical
/// weights. Weights always sum to 1 within decimal tolerance and respect every
/// supplied [`Constraint`].
pub fn optimize(
    objective: AllocationObjective,
    constraints: &[Constraint],
    state: &PortfolioState,
) -> Result<TargetWeights, AllocationError> {
    let as_of = state.as_of;
    let weights = match &objective {
        AllocationObjective::MeanVariance { risk_free_rate } => {
            mean_variance(*risk_free_rate, constraints, state)?
        }
        AllocationObjective::RiskParity => risk_parity(constraints, state)?,
        AllocationObjective::Hrp => hrp(constraints, state)?,
        AllocationObjective::BlackLitterman { views } => {
            black_litterman(views, constraints, state)?
        }
    };
    Ok(TargetWeights {
        as_of,
        objective,
        weights,
    })
}

/// Round continuous [`TargetWeights`] onto an executable whole-lot book
/// (Phase 4 — gap 0004).
///
/// Each instrument's target notional `w·equity` is converted to whole lots at
/// its execution [`Price`] and lot size (`1` when absent), then any residual
/// cash is spent one lot at a time on the instruments with the largest
/// fractional remainders that still fit (iterative largest-remainder /
/// Hamilton rounding, deterministic in `Decimal`).
///
/// The allocated book never exceeds `equity`, and the unspent residual is
/// documented to remain as cash in the account. Returns whole-share
/// [`Quantity`]s per instrument.
pub fn allocate_discrete(
    target: &TargetWeights,
    equity: Money,
    prices: &BTreeMap<InstrumentId, Price>,
    lot_sizes: &BTreeMap<InstrumentId, Quantity>,
) -> Result<BTreeMap<InstrumentId, Quantity>, AllocationError> {
    let equity_amount = equity.amount;
    if equity_amount < Decimal::ZERO {
        return Err(AllocationError::InvalidArgument(
            "equity cannot be negative".into(),
        ));
    }

    // (instrument, lot value, lot size) per target weight, in deterministic
    // BTreeMap order.
    struct Slot {
        instrument: InstrumentId,
        lot_value: Decimal,
        lot_size: Decimal,
    }
    let mut slots = Vec::with_capacity(target.weights.len());
    let mut weights_sum = Decimal::ZERO;
    for (instrument, w) in &target.weights {
        let price = prices.get(instrument).ok_or_else(|| {
            AllocationError::InvalidArgument(format!("missing price for {instrument}"))
        })?;
        if price.value() <= Decimal::ZERO {
            return Err(AllocationError::InvalidArgument(
                "execution price must be positive".into(),
            ));
        }
        let lot = lot_sizes
            .get(instrument)
            .copied()
            .unwrap_or(Quantity::from_u64(1));
        if lot.value() <= Decimal::ZERO {
            return Err(AllocationError::InvalidArgument(
                "lot size must be positive".into(),
            ));
        }
        weights_sum += *w;
        slots.push(Slot {
            instrument: instrument.clone(),
            lot_value: price.value() * lot.value(),
            lot_size: lot.value(),
        });
    }
    if (weights_sum - Decimal::ONE).abs() > EPS {
        return Err(AllocationError::InvalidArgument(
            "target weights do not sum to 1".into(),
        ));
    }

    let mut quantities: BTreeMap<InstrumentId, Quantity> = BTreeMap::new();
    // Largest-remainder ordering: descending fractional lots, tie-broken by
    // the deterministic slot index.
    let mut remainders: Vec<(Decimal, usize)> = Vec::with_capacity(slots.len());
    let mut total_spent = Decimal::ZERO;
    for (i, slot) in slots.iter().enumerate() {
        let w = target.weights[&slot.instrument];
        let raw_lots = w * equity_amount / slot.lot_value;
        let whole = raw_lots.floor().max(Decimal::ZERO);
        let fractional = raw_lots - whole;
        quantities.insert(
            slot.instrument.clone(),
            Quantity::new(slot.lot_size * whole),
        );
        total_spent += slot.lot_value * whole;
        remainders.push((fractional, i));
    }
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    // Iterative largest-remainder: spend residual cash one lot at a time on
    // the best fractional remainders, repeating until nothing fits.
    let mut residual = equity_amount - total_spent;
    for _ in 0..slots.len() {
        let mut spent_any = false;
        for &(_, i) in &remainders {
            let slot = &slots[i];
            if slot.lot_value > residual {
                continue;
            }
            let qty = quantities
                .get_mut(&slot.instrument)
                .expect("quantity slot exists");
            *qty = Quantity::new(qty.value() + slot.lot_size);
            residual -= slot.lot_value;
            spent_any = true;
        }
        if !spent_any {
            break;
        }
    }

    Ok(quantities)
}

// ---------------------------------------------------------------------------
// Input preparation shared by both implemented objectives
// ---------------------------------------------------------------------------

/// Column-vector view of the optimization input.
struct Input {
    instruments: Vec<InstrumentId>,
    /// `(min, max)` weight bound per instrument (defaults `(0, 1)`).
    bounds: Vec<(Decimal, Decimal)>,
    /// Expected return per instrument.
    expected_returns: Vec<Decimal>,
    /// `n × n` covariance matrix.
    covariance: Vec<Vec<Decimal>>,
}

fn prepare_input(
    constraints: &[Constraint],
    state: &PortfolioState,
) -> Result<Input, AllocationError> {
    if state.instruments.is_empty() {
        return Err(AllocationError::InvalidArgument(
            "instrument universe is empty".into(),
        ));
    }
    let n = state.instruments.len();

    let mut min = vec![Decimal::ZERO; n];
    let mut max = vec![Decimal::ONE; n];
    for constraint in constraints {
        let Constraint::WeightBounds {
            instrument,
            min: lo,
            max: hi,
        } = constraint;
        let idx = state
            .instruments
            .iter()
            .position(|id| id == instrument)
            .ok_or_else(|| {
                AllocationError::InvalidArgument(format!(
                    "constraint references instrument {instrument} which is not in the universe"
                ))
            })?;
        if lo > hi {
            return Err(AllocationError::InfeasibleConstraints(format!(
                "weight bounds for {instrument}: min {lo} > max {hi}"
            )));
        }
        min[idx] = *lo;
        max[idx] = *hi;
    }

    // Feasibility: the mandatory weight box must be able to contain a
    // portfolio that sums to 1.
    if min.iter().copied().sum::<Decimal>() > Decimal::ONE {
        return Err(AllocationError::InfeasibleConstraints(
            "sum of minimum weight bounds exceeds 1".into(),
        ));
    }
    if max.iter().copied().sum::<Decimal>() < Decimal::ONE {
        return Err(AllocationError::InfeasibleConstraints(
            "sum of maximum weight bounds is below 1".into(),
        ));
    }

    let mut expected_returns = Vec::with_capacity(n);
    for instrument in &state.instruments {
        expected_returns.push(
            state
                .expected_returns
                .get(instrument)
                .copied()
                .unwrap_or(Decimal::ZERO),
        );
    }

    let mut covariance = Vec::with_capacity(n);
    for instrument in &state.instruments {
        let row = state.covariance.get(instrument).ok_or_else(|| {
            AllocationError::InvalidArgument(format!("missing covariance row for {instrument}"))
        })?;
        let mut row_vec = Vec::with_capacity(n);
        for other in &state.instruments {
            let value = row.get(other).copied().ok_or_else(|| {
                AllocationError::InvalidArgument(format!(
                    "missing covariance entry for ({instrument}, {other})"
                ))
            })?;
            row_vec.push(value);
        }
        covariance.push(row_vec);
    }

    Ok(Input {
        instruments: state.instruments.clone(),
        bounds: min.into_iter().zip(max).collect(),
        expected_returns,
        covariance,
    })
}

/// Exact projection of `x` onto the bounded simplex
/// `{ w : 1ᵀw = 1, min ≤ w ≤ max }` by water-filling (gap 0003): find the
/// unique scalar `λ` with `Σᵢ clamp(xᵢ − λ, minᵢ, maxᵢ) = 1` by binary
/// search, then evaluate the clamps at it.
///
/// Requires feasibility (`Σmin ≤ 1 ≤ Σmax` — enforced in [`prepare_input`]).
/// Deterministic: a fixed number of bisection iterations, so identical
/// inputs give identical outputs.
pub fn project_box_simplex(x: &[Decimal], bounds: &[(Decimal, Decimal)]) -> Vec<Decimal> {
    debug_assert_eq!(x.len(), bounds.len());
    // Bracketing interval. At λ = minᵢ(xᵢ − maxᵢ) every term is at its upper
    // bound (sum = Σmax ≥ 1); at λ = maxᵢ(xᵢ − minᵢ) every term is at its
    // lower bound (sum = Σmin ≤ 1). The root is inside.
    let mut lo = Decimal::MAX;
    let mut hi = Decimal::NEGATIVE_ONE * Decimal::MAX;
    for (xi, (min, max)) in x.iter().zip(bounds) {
        let at_upper = *xi - *max; // λ ≤ at_upper ⇒ term i pinned to max
        let at_lower = *xi - *min; // λ ≥ at_lower ⇒ term i pinned to min
        if at_upper < lo {
            lo = at_upper;
        }
        if at_lower > hi {
            hi = at_lower;
        }
    }
    // 96 bisections drives the interval to ~0.5^96 ≈ 1.6e-29 (below the
    // 28-digit decimal resolution); the returned sum is within EPS of 1.
    for _ in 0..96 {
        let mid = (lo + hi) / Decimal::TWO;
        let sum: Decimal = x
            .iter()
            .zip(bounds)
            .map(|(xi, (min, max))| clamp_weight(*xi - mid, *min, *max))
            .sum();
        // Sum is non-increasing in the shift; root where sum crosses 1.
        if sum > Decimal::ONE {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    x.iter()
        .zip(bounds)
        .map(|(xi, (min, max))| clamp_weight(*xi - hi, *min, *max))
        .collect()
}

fn clamp_weight(x: Decimal, min: Decimal, max: Decimal) -> Decimal {
    if x < min {
        min
    } else if x > max {
        max
    } else {
        x
    }
}

/// True when every weight already lies inside its bounds (with `EPS` slack).
fn within_bounds(weights: &[Decimal], bounds: &[(Decimal, Decimal)]) -> bool {
    weights
        .iter()
        .zip(bounds)
        .all(|(w, (min, max))| *w >= *min - EPS && *w <= *max + EPS)
}

// ---------------------------------------------------------------------------
// Mean-Variance (tangency portfolio)
// ---------------------------------------------------------------------------

fn mean_variance(
    risk_free_rate: Decimal,
    constraints: &[Constraint],
    state: &PortfolioState,
) -> Result<BTreeMap<InstrumentId, Decimal>, AllocationError> {
    let input = prepare_input(constraints, state)?;
    for instrument in &input.instruments {
        if !state.expected_returns.contains_key(instrument) {
            return Err(AllocationError::InvalidArgument(format!(
                "missing expected return for {instrument} (MeanVariance requires it)"
            )));
        }
    }

    // Solve Σ·w = μ − r_f for the tangency portfolio, then normalize.
    let a: Vec<Decimal> = input
        .expected_returns
        .iter()
        .map(|m| *m - risk_free_rate)
        .collect();
    let mut w =
        solve_linear_system(&input.covariance, &a).ok_or(AllocationError::SingularCovariance)?;

    let sum: Decimal = w.iter().copied().sum();
    if sum.is_zero() {
        return Err(AllocationError::SingularCovariance);
    }
    for wi in &mut w {
        *wi /= sum;
    }

    // If the unconstrained tangency already respects every bound it is the
    // exact constrained optimum (the box is inactive) — return it directly.
    if !within_bounds(&w, &input.bounds) {
        w = max_sharpe_box(&input.covariance, &a, &input.bounds, &w)?;
    }
    Ok(input.instruments.into_iter().zip(w).collect())
}

/// Exact box-constrained tangency: maximize the Sharpe ratio
/// `aᵀw / √(wᵀΣw)` over the bounded simplex by projected gradient ascent
/// (gap 0003). The gradient direction is `d = a·(wᵀΣw) − Σw·(aᵀw)` (the
/// numerator of the Sharpe gradient, scaled by a positive factor), followed
/// by exact line search over backtracked step sizes and re-projection onto
/// the bounded simplex. Converges to the KKT point of the box-constrained
/// problem — the tangency *with* the box, which clip-and-renormalize does
/// not find for correlated universes.
fn max_sharpe_box(
    covariance: &[Vec<Decimal>],
    a: &[Decimal],
    bounds: &[(Decimal, Decimal)],
    start: &[Decimal],
) -> Result<Vec<Decimal>, AllocationError> {
    let n = start.len();
    let mut w = project_box_simplex(start, bounds);

    for _ in 0..512 {
        let variance = quad_form(covariance, &w);
        if variance <= Decimal::ZERO {
            return Err(AllocationError::SingularCovariance);
        }
        let num = dot(a, &w);
        if num <= Decimal::ZERO {
            // Degenerate: no positive expected excess return. Fall back to
            // the (documented) box-projected unconstrained tangency.
            return Ok(w);
        }
        let variance_sqrt = variance.sqrt().ok_or(AllocationError::SingularCovariance)?;
        let current_sharpe = num / variance_sqrt;

        let sigma_w = matvec(covariance, &w);
        let mut direction = vec![Decimal::ZERO; n];
        for (i, d) in direction.iter_mut().enumerate() {
            // d = a·v − Σw·q (positive scaling of ∇sharpe).
            *d = a[i] * variance - sigma_w[i] * num;
        }

        // Backtracking line search: scan α = 1, 1/2, ..., 2⁻⁵⁹ and keep the
        // best improving projection.
        let mut candidate = w.clone();
        let mut best_sharpe = current_sharpe;
        let mut improved = false;
        let mut alpha = Decimal::ONE;
        for _ in 0..60 {
            let step: Vec<Decimal> = w
                .iter()
                .zip(&direction)
                .map(|(wi, di)| *wi + alpha * *di)
                .collect();
            let pc = project_box_simplex(&step, bounds);
            let pv = quad_form(covariance, &pc);
            if pv > Decimal::ZERO {
                let pn = dot(a, &pc);
                let pv_sqrt = pv.sqrt().ok_or(AllocationError::SingularCovariance)?;
                let ps = pn / pv_sqrt;
                if ps > best_sharpe {
                    candidate = pc;
                    best_sharpe = ps;
                    improved = true;
                }
            }
            alpha /= Decimal::TWO;
        }

        if !improved {
            // Projected gradient is (numerically) zero: KKT point.
            break;
        }
        w = candidate;
    }

    // Renormalize the KKT point in case accumulated projection slack drifted.
    let sum: Decimal = w.iter().copied().sum();
    if !sum.is_zero() {
        for wi in &mut w {
            *wi /= sum;
        }
    }
    Ok(w)
}

// ---------------------------------------------------------------------------
// Risk Parity (equal risk contribution)
// ---------------------------------------------------------------------------

fn risk_parity(
    constraints: &[Constraint],
    state: &PortfolioState,
) -> Result<BTreeMap<InstrumentId, Decimal>, AllocationError> {
    let input = prepare_input(constraints, state)?;
    let n = input.instruments.len();
    let inv_n = Decimal::ONE / Decimal::from(n as u64);

    // Damped fixed-point iteration for wᵢ(Σw)ᵢ = const, Σw = 1:
    //   F(w) = normalize(1 ⊘ (Σ·w))   (element-wise reciprocal)
    //   w ← (1 − α)w + α·F(w), α = 0.5
    // Damping is required — the undamped map 2-cycles even for a diagonal
    // covariance (e.g. diag(0.04, 0.16) alternates [0.8, 0.2] ↔ [0.5, 0.5]).
    // Bounds are enforced exactly by projecting every iterate onto the
    // bounded simplex (gap 0003): at the fixed point the free variables have
    // equal risk contribution and the pinned variables sit at their bounds —
    // the KKT system of the box-constrained risk-parity problem.
    let mut w = vec![inv_n; n];
    for _ in 0..512 {
        let sigma_w = matvec(&input.covariance, &w);

        // Element-wise reciprocal of the marginal risk vector.
        let mut recip = Vec::with_capacity(n);
        let mut beta = Decimal::ZERO;
        let mut feasible = true;
        for &v in &sigma_w {
            if v <= Decimal::ZERO {
                // Marginal risk must be positive for risk parity; a
                // non-positive-definite covariance is a caller error.
                feasible = false;
                break;
            }
            let r = Decimal::ONE / v;
            recip.push(r);
            beta += r;
        }
        if !feasible {
            return Err(AllocationError::SingularCovariance);
        }

        let mut next = Vec::with_capacity(n);
        for (i, r) in recip.iter().enumerate() {
            let target = *r / beta;
            // Damped update with α = 0.5.
            next.push(w[i] + (target - w[i]) * Decimal::new(5, 1));
        }
        next = project_box_simplex(&next, &input.bounds);

        let mut max_delta = Decimal::ZERO;
        for (a, b) in w.iter().zip(&next) {
            let delta = (*a - *b).abs();
            if delta > max_delta {
                max_delta = delta;
            }
        }
        w = next;
        if max_delta <= EPS {
            break;
        }
    }

    Ok(input.instruments.into_iter().zip(w).collect())
}

// ---------------------------------------------------------------------------
// Hierarchical Risk Parity (Phase 2 — gap 0001)
// ---------------------------------------------------------------------------

/// HRP (López de Prado): cluster the covariance matrix by correlation
/// distance with single-linkage agglomeration, quasi-diagonalize the
/// dendrogram, then allocate risk by recursive bisection. All clustering math
/// is exact `Decimal` (square roots via `rust_decimal::Decimal::sqrt`); the
/// fixed-point tolerance for the leaf-order work is `EPS`.
fn hrp(
    constraints: &[Constraint],
    state: &PortfolioState,
) -> Result<BTreeMap<InstrumentId, Decimal>, AllocationError> {
    let input = prepare_input(constraints, state)?;
    let n = input.instruments.len();

    if n == 1 {
        let w = project_box_simplex(&[Decimal::ONE], &input.bounds);
        return Ok(input.instruments.into_iter().zip(w).collect());
    }

    // 1. Correlation distance matrix.
    let distance = correlation_distance(&input.covariance)?;

    // 2. Single-linkage dendrogram: n−1 merge steps of cluster ids (leaves
    //    0..n; the cluster formed at step k gets id n+k).
    let merges = single_linkage(&distance);

    // 3. Quasi-diagonalization: the leaf order induced by the dendrogram.
    let order = quasi_diagonalize(n, &merges);

    // 4. Recursive bisection risk allocation.
    let mut weights = vec![Decimal::ZERO; n];
    recursive_bisect(&input.covariance, &order, &mut weights, Decimal::ONE)?;

    // 5. Exact projection onto the bounded simplex (bounds handling).
    let w = project_box_simplex(&weights, &input.bounds);
    Ok(input.instruments.into_iter().zip(w).collect())
}

/// Correlation-distance matrix `d_ij = √(0.5·(1 − ρ_ij))` with
/// `ρ_ij = Σ_ij / √(Σ_ii·Σ_jj)`. Requires a strictly positive diagonal.
fn correlation_distance(covariance: &[Vec<Decimal>]) -> Result<Vec<Vec<Decimal>>, AllocationError> {
    let n = covariance.len();
    let mut vars = Vec::with_capacity(n);
    for (i, row) in covariance.iter().enumerate() {
        if row[i] <= Decimal::ZERO {
            return Err(AllocationError::InvalidArgument(
                "HRP requires a strictly positive covariance diagonal".into(),
            ));
        }
        vars.push(row[i]);
    }
    let mut distance = vec![vec![Decimal::ZERO; n]; n];
    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            let rho = covariance[i][j]
                / (vars[i] * vars[j])
                    .sqrt()
                    .ok_or(AllocationError::SingularCovariance)?;
            // Clamp the correlation to the valid range for numerical safety.
            let mut half = (Decimal::ONE - rho) / Decimal::TWO;
            if half < Decimal::ZERO {
                half = Decimal::ZERO;
            }
            if half > Decimal::ONE {
                half = Decimal::ONE;
            }
            distance[i][j] = half.sqrt().ok_or(AllocationError::SingularCovariance)?;
        }
    }
    Ok(distance)
}

/// Agglomerative single-linkage clustering: repeatedly merge the closest pair
/// of clusters (distance = min over member leaf pairs), returning the n−1
/// merge steps `(a, b)` in order. Ties break deterministically by cluster id.
fn single_linkage(distance: &[Vec<Decimal>]) -> Vec<(usize, usize)> {
    let n = distance.len();
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut active: Vec<usize> = (0..n).collect();
    let mut merges = Vec::with_capacity(n - 1);
    let mut next_id = n;

    while active.len() > 1 {
        let mut best_a = usize::MAX;
        let mut best_b = usize::MAX;
        let mut best_d = Decimal::MAX;
        for (ai, &a) in active.iter().enumerate() {
            for &b in &active[ai + 1..] {
                let d = cluster_distance(&members[a], &members[b], distance);
                if d < best_d {
                    best_d = d;
                    best_a = a;
                    best_b = b;
                }
            }
        }
        let (lo, hi) = (best_a.min(best_b), best_a.max(best_b));
        merges.push((lo, hi));

        let mut merged = members[lo].clone();
        merged.extend(members[hi].iter().copied());
        merged.sort_unstable();
        members.push(merged);

        active.retain(|&c| c != hi);
        let lo_idx = active
            .iter()
            .position(|&c| c == lo)
            .expect("cluster present");
        active[lo_idx] = next_id;
        next_id += 1;
    }
    merges
}

fn cluster_distance(a: &[usize], b: &[usize], distance: &[Vec<Decimal>]) -> Decimal {
    let mut best = Decimal::MAX;
    for &x in a {
        for &y in b {
            let d = distance[x][y];
            if d < best {
                best = d;
            }
        }
    }
    best
}

/// Leaf order from the dendrogram (López de Prado quasi-diagonalization):
/// clusters merged together are contiguous, with the earlier-formed (older)
/// child visited first.
fn quasi_diagonalize(n: usize, merges: &[(usize, usize)]) -> Vec<usize> {
    let mut order = Vec::with_capacity(n);
    let root = 2 * n - 2; // the cluster formed by the final merge
    collect_order(root, merges, &mut order);
    order
}

fn collect_order(node: usize, merges: &[(usize, usize)], order: &mut Vec<usize>) {
    if node < merges.len() + 1 {
        // Leaf id (0..n). (Internal nodes have id ≥ n.)
        order.push(node);
        return;
    }
    let step = node - merges.len() - 1;
    let (a, b) = merges[step];
    collect_order(a, merges, order);
    collect_order(b, merges, order);
}

/// Recursively allocate the cluster risk budget down the quasi-diagonal
/// ordering: split in half, weigh each half by inverse cluster variance
/// (`1/vᵢ / Σ(1/vⱼ)`), and recurse. The cluster variance uses the
/// inverse-variance portfolio inside the cluster.
fn recursive_bisect(
    covariance: &[Vec<Decimal>],
    order: &[usize],
    weights: &mut [Decimal],
    cluster_weight: Decimal,
) -> Result<(), AllocationError> {
    if order.len() == 1 {
        weights[order[0]] = cluster_weight;
        return Ok(());
    }
    let mid = order.len() / 2;
    let left = &order[..mid];
    let right = &order[mid..];
    let v_left = cluster_variance(covariance, left)?;
    let v_right = cluster_variance(covariance, right)?;
    let inv_left = Decimal::ONE / v_left;
    let inv_right = Decimal::ONE / v_right;
    let alpha = inv_left / (inv_left + inv_right);
    recursive_bisect(covariance, left, weights, cluster_weight * alpha)?;
    recursive_bisect(
        covariance,
        right,
        weights,
        cluster_weight * (Decimal::ONE - alpha),
    )
}

/// Variance of the inverse-variance sub-portfolio restricted to `members`.
fn cluster_variance(
    covariance: &[Vec<Decimal>],
    members: &[usize],
) -> Result<Decimal, AllocationError> {
    let mut inv = Vec::with_capacity(members.len());
    let mut inv_sum = Decimal::ZERO;
    for &i in members {
        let one_over = Decimal::ONE / covariance[i][i];
        inv.push(one_over);
        inv_sum += one_over;
    }
    for w in &mut inv {
        *w /= inv_sum;
    }
    let mut variance = Decimal::ZERO;
    for (a, &ma) in members.iter().enumerate() {
        let mut row = Decimal::ZERO;
        for (b, &mb) in members.iter().enumerate() {
            row += inv[b] * covariance[ma][mb];
        }
        variance += inv[a] * row;
    }
    if variance <= Decimal::ZERO {
        return Err(AllocationError::SingularCovariance);
    }
    Ok(variance)
}

// ---------------------------------------------------------------------------
// Black-Litterman (Phase 3 — gap 0002)
// ---------------------------------------------------------------------------

/// Black-Litterman blended allocation.
///
/// 1. Reverse-optimize the market-implied (prior) returns from the benchmark
///    weights: `π = τ·Σ·w_mkt`.
/// 2. Blend the views into the posterior
///    `μ_post = ((τΣ)⁻¹ + PᵀΩ⁻¹P)⁻¹ · ((τΣ)⁻¹π + PᵀΩ⁻¹q)`.
/// 3. Return the posterior-covariance tangency `w ∝ Σ_post⁻¹·μ_post`
///    (equivalently `A·μ_post` with `A = Σ_post⁻¹`), normalized to sum to 1
///    and projected onto the bounded simplex.
///
/// With no views (`P = 0`) the posterior mean collapses back to `π` and the
/// allocation returns the benchmark weights unchanged.
fn black_litterman(
    views: &BlackLittermanViews,
    constraints: &[Constraint],
    state: &PortfolioState,
) -> Result<BTreeMap<InstrumentId, Decimal>, AllocationError> {
    let input = prepare_input(constraints, state)?;
    let n = input.instruments.len();

    if views.tau <= Decimal::ZERO {
        return Err(AllocationError::InvalidArgument(
            "BlackLitterman tau must be positive".into(),
        ));
    }

    // Market weights: every universe instrument must have one; normalize to 1.
    let mut w_mkt = Vec::with_capacity(n);
    let mut mkt_sum = Decimal::ZERO;
    for instrument in &input.instruments {
        let w = views
            .market_weights
            .get(instrument)
            .copied()
            .ok_or_else(|| {
                AllocationError::InvalidArgument(format!("missing market weight for {instrument}"))
            })?;
        w_mkt.push(w);
        mkt_sum += w;
    }
    if mkt_sum.is_zero() {
        return Err(AllocationError::InvalidArgument(
            "market weights sum to zero".into(),
        ));
    }
    for w in &mut w_mkt {
        *w /= mkt_sum;
    }

    // π = τΣ·w_mkt and (τΣ)⁻¹.
    let tau_sigma = scale_matrix(&input.covariance, views.tau);
    let pi = matvec(&tau_sigma, &w_mkt);
    let inv_tau_sigma = invert_matrix(&tau_sigma).ok_or(AllocationError::SingularCovariance)?;

    // Build P (k×n), q (k), and diagonal Ω⁻¹ from the views.
    let k = views.views.len();
    let mut p = vec![vec![Decimal::ZERO; n]; k];
    let mut q = vec![Decimal::ZERO; k];
    let mut omega_inv = vec![vec![Decimal::ZERO; k]; k];
    for (r, view) in views.views.iter().enumerate() {
        if view.instruments.len() != view.coefficients.len() {
            return Err(AllocationError::InvalidArgument(
                "view instruments/coefficients length mismatch".into(),
            ));
        }
        if view.uncertainty <= Decimal::ZERO {
            return Err(AllocationError::InvalidArgument(
                "view uncertainty must be positive".into(),
            ));
        }
        for (instrument, coefficient) in view.instruments.iter().zip(&view.coefficients) {
            let idx = input
                .instruments
                .iter()
                .position(|id| id == instrument)
                .ok_or_else(|| {
                    AllocationError::InvalidArgument(format!(
                        "view references instrument {instrument} which is not in the universe"
                    ))
                })?;
            p[r][idx] += *coefficient;
        }
        q[r] = view.mean;
        omega_inv[r][r] = Decimal::ONE / (view.uncertainty * view.uncertainty);
    }

    // M = PᵀΩ⁻¹P, b = PᵀΩ⁻¹q (the view information contribution).
    let mut m = vec![vec![Decimal::ZERO; n]; n];
    let mut b = vec![Decimal::ZERO; n];
    for r in 0..k {
        let w_rr = omega_inv[r][r];
        for i in 0..n {
            b[i] += p[r][i] * w_rr * q[r];
            for j in 0..n {
                m[i][j] += p[r][i] * w_rr * p[r][j];
            }
        }
    }

    // A = (τΣ)⁻¹ + PᵀΩ⁻¹P, rhs = (τΣ)⁻¹π + PᵀΩ⁻¹q.
    let mut a = vec![vec![Decimal::ZERO; n]; n];
    for i in 0..n {
        for j in 0..n {
            a[i][j] = inv_tau_sigma[i][j] + m[i][j];
        }
    }
    let inv_pi = matvec(&inv_tau_sigma, &pi);
    let rhs: Vec<Decimal> = inv_pi.iter().zip(&b).map(|(x, y)| *x + *y).collect();

    // Posterior mean μ_post = A⁻¹·rhs, then the posterior tangency w = A·μ_post.
    let mu_post = solve_linear_system(&a, &rhs).ok_or(AllocationError::SingularCovariance)?;
    let w_raw = matvec(&a, &mu_post);

    let sum: Decimal = w_raw.iter().copied().sum();
    if sum.is_zero() {
        return Err(AllocationError::SingularCovariance);
    }
    let mut w: Vec<Decimal> = w_raw.iter().map(|x| *x / sum).collect();
    w = project_box_simplex(&w, &input.bounds);
    Ok(input.instruments.into_iter().zip(w).collect())
}

// ---------------------------------------------------------------------------
// Linear algebra helpers (pure Decimal)
// ---------------------------------------------------------------------------

/// `matrix · vector` product.
fn matvec(matrix: &[Vec<Decimal>], vector: &[Decimal]) -> Vec<Decimal> {
    matrix
        .iter()
        .map(|row| {
            row.iter()
                .zip(vector)
                .map(|(a, b)| *a * *b)
                .sum::<Decimal>()
        })
        .collect()
}

/// Dot product of two equal-length vectors.
fn dot(a: &[Decimal], b: &[Decimal]) -> Decimal {
    a.iter().zip(b).map(|(x, y)| *x * *y).sum()
}

/// Quadratic form `wᵀ·matrix·w`.
fn quad_form(matrix: &[Vec<Decimal>], w: &[Decimal]) -> Decimal {
    dot(w, &matvec(matrix, w))
}

/// Multiply every entry of `matrix` by the scalar `scale`.
fn scale_matrix(matrix: &[Vec<Decimal>], scale: Decimal) -> Vec<Vec<Decimal>> {
    matrix
        .iter()
        .map(|row| row.iter().map(|x| *x * scale).collect())
        .collect()
}

/// Matrix inverse by Gauss-Jordan elimination with partial pivoting, entirely
/// in `Decimal`. Returns `None` for a singular matrix.
fn invert_matrix(matrix: &[Vec<Decimal>]) -> Option<Vec<Vec<Decimal>>> {
    let n = matrix.len();
    let mut aug = matrix.to_vec();
    let mut inv: Vec<Vec<Decimal>> = (0..n)
        .map(|i| {
            let mut row = vec![Decimal::ZERO; n];
            row[i] = Decimal::ONE;
            row
        })
        .collect();

    for col in 0..n {
        let mut pivot = col;
        for row in (col + 1)..n {
            if aug[row][col].abs() > aug[pivot][col].abs() {
                pivot = row;
            }
        }
        if aug[pivot][col].is_zero() {
            return None; // singular
        }
        aug.swap(col, pivot);
        inv.swap(col, pivot);

        let pv = aug[col][col];
        for x in &mut aug[col][col..] {
            *x /= pv;
        }
        for x in &mut inv[col] {
            *x /= pv;
        }

        let pivot_row = aug[col][col..].to_vec();
        let pivot_inv = inv[col].clone();
        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = aug[row][col];
            if factor.is_zero() {
                continue;
            }
            for (j, &entry) in pivot_row.iter().enumerate() {
                aug[row][col + j] -= factor * entry;
            }
            for (j, &entry) in pivot_inv.iter().enumerate() {
                inv[row][j] -= factor * entry;
            }
        }
    }
    Some(inv)
}

/// Solves `matrix · x = rhs` for a square matrix by Gaussian elimination with
/// partial pivoting, entirely in `Decimal`. Returns `None` for a singular
/// matrix.
fn solve_linear_system(matrix: &[Vec<Decimal>], rhs: &[Decimal]) -> Option<Vec<Decimal>> {
    let n = rhs.len();
    let mut m = matrix.to_vec();
    let mut b = rhs.to_vec();

    for col in 0..n {
        // Partial pivot: row with the largest absolute value in this column.
        let mut pivot = col;
        for row in (col + 1)..n {
            if m[row][col].abs() > m[pivot][col].abs() {
                pivot = row;
            }
        }
        if m[pivot][col].is_zero() {
            return None; // singular
        }
        m.swap(col, pivot);
        b.swap(col, pivot);

        let pv = m[col][col];
        for x in &mut m[col][col..] {
            *x /= pv;
        }
        b[col] /= pv;

        // Snapshot the normalized pivot row so we can mutate the other rows
        // without aliasing `m[col]`.
        let pivot_row = m[col][col..].to_vec();
        let pivot_rhs = b[col];

        // Eliminate every other row.
        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = m[row][col];
            if factor.is_zero() {
                continue;
            }
            for (j, &pivot) in pivot_row.iter().enumerate() {
                m[row][col + j] -= factor * pivot;
            }
            b[row] -= factor * pivot_rhs;
        }
    }
    Some(b)
}
