//! Multi-period optimization (MPO) — plan 0007 Track 4 Phase 11 (gap 0013).
//!
//! The single-period [`optimize`] solve targets one set of weights, but
//! rebalancing a held book onto that target is itself a decision: every
//! trade pays a spread plus statutory frictions, and moving the whole book
//! in one step is usually not the cheapest way to get there. The multi-period
//! optimizer sequences the rebalance over `T` periods so each period's book
//! moves only a bounded distance toward the single-period target, paying a
//! documented friction cost (quadratic in turnover plus a linear penalty) per
//! step. It is the domain foundation for the future MCP gateway tool
//! `tantra_multi_period_optimize`.
//!
//! # Algorithm
//!
//! 1. **Validate**: at least one period is required
//!    ([`AllocationError::InvalidArgument`] otherwise).
//! 2. **Bounds**: build the ordered per-instrument `(min, max)` box exactly as
//!    the single-period solver's (private) input preparation does — start
//!    from `(0, 1)` per instrument, apply every
//!    [`Constraint::WeightBounds`] by instrument index, and validate
//!    `min ≤ max`, `Σmin ≤ 1`, `Σmax ≥ 1`, returning the identical
//!    [`AllocationError`]s on violation.
//! 3. **Book**: `w_prev = project_box_simplex(current_weights)` — the held
//!    book, projected onto the bounded simplex (missing instruments read as
//!    zero weight).
//! 4. **Per period `t`**: re-solve the single-period target with [`optimize`],
//!    then blend from the current book toward it — start `α = 1` and halve it
//!    (up to [`MpoConfig::max_iterations`] times) until the *projected* blend
//!    `project_box_simplex(w_prev + α·(target − w_prev))` has L1 turnover
//!    `Σᵢ|wᵢ − w_prev,ᵢ|` at or below
//!    [`MpoConfig::max_turnover_per_period`] (best effort: when even the
//!    smallest `α` still exceeds the cap, the smallest-`α` candidate is taken
//!    — deterministically, the last candidate examined). Record the
//!    [`MpoStep`]: the projected target weights, the L1 turnover, and the
//!    friction cost `tc·turnover² + turnover_penalty·turnover`. Advance
//!    `w_prev = w_t` for the next period.
//! 5. **Aggregate** the per-step friction and turnover into [`MpoResult`].
//!
//! Everything is exact `rust_decimal::Decimal` (the `maths` feature supplies
//! `.abs()` and `.powi(2)`); there are no floats, no I/O, and no clock —
//! time enters only through the caller-supplied `as_of` on [`PortfolioState`].
//! Identical inputs produce a byte-identical [`MpoResult`].

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use std::collections::BTreeMap;

use crate::{
    AllocationError, AllocationObjective, Constraint, InstrumentId, PortfolioState, optimize,
    project_box_simplex,
};

/// Multi-period rebalancing configuration (plan 0007 Track 4 Phase 11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpoConfig {
    /// Number of rebalancing periods `T` (>= 1).
    pub periods: usize,
    /// Quadratic transaction-cost coefficient: cost = tc * turnover².
    pub transaction_cost: Decimal,
    /// Linear turnover penalty per unit of L1 turnover.
    pub turnover_penalty: Decimal,
    /// Optional cap on L1 turnover per period (None = unlimited).
    pub max_turnover_per_period: Option<Decimal>,
    /// Max iterations of the turnover-blend refinement loop.
    pub max_iterations: usize,
}

impl Default for MpoConfig {
    fn default() -> Self {
        Self {
            periods: 3,
            transaction_cost: Decimal::new(1, 3), // 0.001
            turnover_penalty: Decimal::new(1, 4), // 0.0001
            max_turnover_per_period: Some(Decimal::new(5, 1)), // 0.5
            max_iterations: 64,
        }
    }
}

/// One rebalancing step of the MPO sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpoStep {
    /// Zero-based period index.
    pub period: usize,
    /// Target weights after this period's rebalance (sum to 1, within bounds).
    pub target_weights: BTreeMap<InstrumentId, Decimal>,
    /// L1 turnover of this rebalance: sum(|w_t - w_{t-1}|).
    pub turnover: Decimal,
    /// Friction cost of this rebalance: tc*turnover² + turnover_penalty*turnover.
    pub transaction_cost_incurred: Decimal,
}

/// Full MPO result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpoResult {
    /// One step per period, in order.
    pub steps: Vec<MpoStep>,
    /// Sum of every step's friction cost.
    pub total_transaction_cost: Decimal,
    /// Sum of every step's L1 turnover.
    pub total_turnover: Decimal,
}

/// Multi-period portfolio rebalancing with transaction costs and a turnover cap.
pub fn multi_period_optimize(
    objective: AllocationObjective,
    constraints: &[Constraint],
    state: &PortfolioState,
    config: &MpoConfig,
) -> Result<MpoResult, AllocationError> {
    if config.periods == 0 {
        return Err(AllocationError::InvalidArgument(
            "periods must be at least 1".into(),
        ));
    }
    let bounds = ordered_bounds(constraints, state)?;

    // The projected current book; `state.current_weights` may not itself be
    // feasible (missing instruments read as zero), so project it exactly once.
    let current_vec = weight_vector(state, &state.current_weights);
    let mut w_prev = project_box_simplex(&current_vec, &bounds);

    let mut steps = Vec::with_capacity(config.periods);
    let mut total_transaction_cost = Decimal::ZERO;
    let mut total_turnover = Decimal::ZERO;

    for t in 0..config.periods {
        // The single-period target is stateless with respect to the current
        // book, so it is identical every period; recomputing it keeps the
        // sequence trivially auditable and mirrors live re-solves.
        let target = optimize(objective.clone(), constraints, state)?;
        let target_vec = weight_vector(state, &target.weights);

        // Blend from the current book toward the target, halving alpha until
        // the projected blend's L1 turnover fits under the per-period cap.
        // Each failing candidate is kept as the deterministic best-effort
        // fallback, so w_t is always the smallest-alpha candidate examined.
        let mut w_t: Vec<Decimal> = Vec::with_capacity(state.instruments.len());
        let mut alpha = Decimal::ONE;
        for _ in 0..config.max_iterations.max(1) {
            let blend: Vec<Decimal> = w_prev
                .iter()
                .zip(&target_vec)
                .map(|(p, tgt)| *p + alpha * (*tgt - *p))
                .collect();
            let candidate = project_box_simplex(&blend, &bounds);
            let candidate_turnover = l1_distance(&candidate, &w_prev);
            match config.max_turnover_per_period {
                Some(cap) if candidate_turnover > cap => {
                    // Over the cap: keep this candidate as the fallback and
                    // shrink toward the current book.
                    w_t = candidate;
                    alpha /= Decimal::TWO;
                }
                _ => {
                    // No cap, or the cap is met — accept this blend.
                    w_t = candidate;
                    break;
                }
            }
        }

        let turnover = l1_distance(&w_t, &w_prev);
        let cost = config.transaction_cost * turnover.powi(2) + config.turnover_penalty * turnover;

        let target_weights: BTreeMap<InstrumentId, Decimal> = state
            .instruments
            .iter()
            .cloned()
            .zip(w_t.iter().copied())
            .collect();
        steps.push(MpoStep {
            period: t,
            target_weights,
            turnover,
            transaction_cost_incurred: cost,
        });
        total_transaction_cost += cost;
        total_turnover += turnover;
        w_prev = w_t;
    }

    Ok(MpoResult {
        steps,
        total_transaction_cost,
        total_turnover,
    })
}

/// Ordered `(min, max)` bounds per instrument, replicating the single-period
/// solver's constraint derivation (its `prepare_input` is private to `lib.rs`).
fn ordered_bounds(
    constraints: &[Constraint],
    state: &PortfolioState,
) -> Result<Vec<(Decimal, Decimal)>, AllocationError> {
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
    Ok(min.into_iter().zip(max).collect())
}

/// Per-instrument vector ordered by `state.instruments`; weights missing from
/// the map read as zero.
fn weight_vector(
    state: &PortfolioState,
    weights: &BTreeMap<InstrumentId, Decimal>,
) -> Vec<Decimal> {
    state
        .instruments
        .iter()
        .map(|id| weights.get(id).copied().unwrap_or(Decimal::ZERO))
        .collect()
}

/// L1 (taxicab) distance between two equal-length vectors.
fn l1_distance(a: &[Decimal], b: &[Decimal]) -> Decimal {
    a.iter().zip(b).map(|(x, y)| (*x - *y).abs()).sum()
}
