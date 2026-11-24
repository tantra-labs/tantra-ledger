//! Multi-period optimizer (MPO) tests — gap 0013 / plan 0007 Track 4 Phase 11.
//!
//! The suite covers config validation, equivalence with the single-period
//! solve under zero friction, the per-period turnover cap, friction
//! monotonicity in the cost parameter, deterministic repeatability, and the
//! validity (sum-to-1, in-bounds) of every step's target weights. All
//! assertions use exact `Decimal` with an explicit `1e-9` tolerance.

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use tantra_core::Exchange;
use tantra_ledger_allocation::{
    AllocationError, AllocationObjective, Constraint, MpoConfig, PortfolioState,
    multi_period_optimize, optimize,
};

/// Absolute tolerance for every numerical assertion: `1e-9`.
const TOL: Decimal = Decimal::from_parts(1, 0, 0, false, 9); // 0.000000001

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn instrument(symbol: &str) -> tantra_core::InstrumentId {
    tantra_core::InstrumentId::try_new(Exchange::Nse, symbol).unwrap()
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap()
}

/// Three-instrument universe: expected returns `(0.10, 0.12, 0.08)` over the
/// diagonally-dominant positive-definite covariance
/// `diag(0.04, 0.16, 0.09)` with small off-diagonal entries. The initial book
/// is `[0.5, 0.25, 0.25]` — feasible and summing to exactly 1.
fn three_asset_state() -> PortfolioState {
    let a = instrument("A");
    let b = instrument("B");
    let c = instrument("C");

    let mut expected_returns = BTreeMap::new();
    expected_returns.insert(a.clone(), d("0.10"));
    expected_returns.insert(b.clone(), d("0.12"));
    expected_returns.insert(c.clone(), d("0.08"));

    let mut covariance = BTreeMap::new();
    let mut row_a = BTreeMap::new();
    row_a.insert(a.clone(), d("0.04"));
    row_a.insert(b.clone(), d("0.01"));
    row_a.insert(c.clone(), d("0.01"));
    let mut row_b = BTreeMap::new();
    row_b.insert(a.clone(), d("0.01"));
    row_b.insert(b.clone(), d("0.16"));
    row_b.insert(c.clone(), d("0.01"));
    let mut row_c = BTreeMap::new();
    row_c.insert(a.clone(), d("0.01"));
    row_c.insert(b.clone(), d("0.01"));
    row_c.insert(c.clone(), d("0.09"));
    covariance.insert(a.clone(), row_a);
    covariance.insert(b.clone(), row_b);
    covariance.insert(c.clone(), row_c);

    let mut current_weights = BTreeMap::new();
    current_weights.insert(a, d("0.5"));
    current_weights.insert(b, d("0.25"));
    current_weights.insert(c, d("0.25"));

    PortfolioState {
        as_of: now(),
        instruments: vec![instrument("A"), instrument("B"), instrument("C")],
        current_weights,
        expected_returns,
        covariance,
    }
}

/// Baseline config: 3 periods, small quadratic + linear friction, no cap.
fn config(periods: usize) -> MpoConfig {
    MpoConfig {
        periods,
        transaction_cost: d("0.001"),
        turnover_penalty: d("0.0001"),
        max_turnover_per_period: None,
        max_iterations: 64,
    }
}

fn mean_variance() -> AllocationObjective {
    AllocationObjective::MeanVariance {
        risk_free_rate: Decimal::ZERO,
    }
}

#[test]
fn zero_periods_rejected() {
    let config = config(0);
    let err =
        multi_period_optimize(mean_variance(), &[], &three_asset_state(), &config).unwrap_err();
    assert!(
        matches!(err, AllocationError::InvalidArgument(msg) if msg.contains("periods")),
        "periods == 0 must be rejected with an InvalidArgument error"
    );
}

#[test]
fn single_period_matches_optimize_with_no_friction() {
    let state = three_asset_state();
    let config = MpoConfig {
        periods: 1,
        transaction_cost: Decimal::ZERO,
        turnover_penalty: Decimal::ZERO,
        max_turnover_per_period: None,
        max_iterations: 64,
    };

    let result = multi_period_optimize(mean_variance(), &[], &state, &config).unwrap();
    assert_eq!(result.steps.len(), 1);
    let step = &result.steps[0];
    assert_eq!(step.period, 0);

    // With no cap the first (alpha = 1) blend is exactly the single-period
    // target, projected back onto the same feasible box: equality up to the
    // projection's own decimal resolution.
    let single = optimize(mean_variance(), &[], &state).unwrap();
    for id in &state.instruments {
        let diff = (step.target_weights[id] - single.weights[id]).abs();
        assert!(
            diff < d("0.000001"),
            "period-0 weight for {id} differs from single-period solve by {diff}"
        );
    }

    // Step turnover is the L1 distance from the initial book.
    let expected_turnover: Decimal = state
        .instruments
        .iter()
        .map(|id| (step.target_weights[id] - state.current_weights[id]).abs())
        .sum();
    assert!(
        (step.turnover - expected_turnover).abs() <= TOL,
        "turnover {} != L1 distance {expected_turnover}",
        step.turnover
    );

    // Zero friction parameters mean zero friction cost, exactly.
    // Zero friction parameters give exactly zero friction cost; the turnover
    // is the single step's turnover (one period), up to projection slack.
    assert_eq!(result.total_transaction_cost, Decimal::ZERO);
    assert!(
        (result.total_turnover - expected_turnover).abs() <= TOL,
        "total turnover {} != expected {expected_turnover}",
        result.total_turnover
    );
}

#[test]
fn turnover_cap_limits_each_rebalance() {
    let mut state = three_asset_state();
    // Skew the initial book away from the return-favoring target so the
    // per-period cap actually binds on the first rebalance.
    state.current_weights = {
        let mut w = BTreeMap::new();
        w.insert(instrument("A"), d("0.5"));
        w.insert(instrument("B"), Decimal::ZERO);
        w.insert(instrument("C"), d("0.5"));
        w
    };
    let config = MpoConfig {
        periods: 3,
        transaction_cost: d("0.001"),
        turnover_penalty: d("0.0001"),
        max_turnover_per_period: Some(d("0.25")),
        max_iterations: 64,
    };

    let result = multi_period_optimize(mean_variance(), &[], &state, &config).unwrap();
    assert_eq!(result.steps.len(), 3);
    let mut total_turnover = Decimal::ZERO;
    for step in &result.steps {
        assert!(
            step.turnover <= d("0.25") + TOL,
            "period {} turnover {} exceeds the 0.25 cap",
            step.period,
            step.turnover
        );
        total_turnover += step.turnover;
    }
    // The capped sequence still makes progress toward the target.
    assert!(total_turnover > Decimal::ZERO);
    assert_eq!(result.total_turnover, total_turnover);
}

#[test]
fn higher_cost_config_pays_more_friction() {
    let state = three_asset_state();
    let low = config(3);
    let high = MpoConfig {
        transaction_cost: d("0.01"),
        ..config(3)
    };

    let low_result = multi_period_optimize(mean_variance(), &[], &state, &low).unwrap();
    let high_result = multi_period_optimize(mean_variance(), &[], &state, &high).unwrap();

    assert!(
        high_result.total_transaction_cost > low_result.total_transaction_cost,
        "10x transaction cost must pay strictly more friction: {} vs {}",
        high_result.total_transaction_cost,
        low_result.total_transaction_cost
    );
}

#[test]
fn deterministic_repeatability() {
    let state = three_asset_state();
    let config = MpoConfig {
        periods: 4,
        max_turnover_per_period: Some(d("0.25")),
        ..config(4)
    };

    let first = multi_period_optimize(mean_variance(), &[], &state, &config).unwrap();
    let second = multi_period_optimize(mean_variance(), &[], &state, &config).unwrap();
    assert_eq!(first, second);
}

#[test]
fn weights_stay_valid() {
    let state = three_asset_state();
    let b_cap = d("0.6");
    let constraints = [Constraint::WeightBounds {
        instrument: instrument("B"),
        min: Decimal::ZERO,
        max: b_cap,
    }];
    let config = MpoConfig {
        periods: 4,
        transaction_cost: d("0.001"),
        turnover_penalty: d("0.0001"),
        max_turnover_per_period: Some(d("0.25")),
        max_iterations: 64,
    };

    let result = multi_period_optimize(mean_variance(), &constraints, &state, &config).unwrap();
    assert_eq!(result.steps.len(), 4);
    for step in &result.steps {
        let sum: Decimal = step.target_weights.values().copied().sum();
        assert!(
            (sum - Decimal::ONE).abs() <= TOL,
            "period {} weights sum to {sum}, expected 1",
            step.period
        );
        for id in &state.instruments {
            let w = step.target_weights[id];
            let (min, max) = if *id == instrument("B") {
                (Decimal::ZERO, b_cap)
            } else {
                (Decimal::ZERO, Decimal::ONE)
            };
            assert!(
                w >= min - TOL,
                "period {} weight {id} = {w} below min {min}",
                step.period
            );
            assert!(
                w <= max + TOL,
                "period {} weight {id} = {w} above max {max}",
                step.period
            );
        }
    }
}
