//! Golden-vector and error-path tests for `tantra-ledger-allocation`.

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use tantra_core::{Currency, Exchange, InstrumentId, Money, Price, Quantity};
use tantra_ledger_allocation::{
    AllocationError, AllocationObjective, BlackLittermanViews, Constraint, PortfolioState,
    TargetWeights, View, allocate_discrete, optimize,
};
use tantra_ledger_portfolio::{AccountState, HeldPosition, Marks, PositionSide};

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn instrument(symbol: &str) -> InstrumentId {
    InstrumentId::new(Exchange::Nse, symbol)
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap()
}

/// Two-asset state: A has return 0.10, B 0.12, with the golden covariance
/// `[[1, 0.5], [0.5, 1]]`.
fn two_asset_state() -> PortfolioState {
    let mut expected_returns = BTreeMap::new();
    expected_returns.insert(instrument("A"), d("0.10"));
    expected_returns.insert(instrument("B"), d("0.12"));

    let mut covariance = BTreeMap::new();
    let mut row_a = BTreeMap::new();
    row_a.insert(instrument("A"), d("1"));
    row_a.insert(instrument("B"), d("0.5"));
    let mut row_b = BTreeMap::new();
    row_b.insert(instrument("A"), d("0.5"));
    row_b.insert(instrument("B"), d("1"));
    covariance.insert(instrument("A"), row_a);
    covariance.insert(instrument("B"), row_b);

    PortfolioState {
        as_of: now(),
        instruments: vec![instrument("A"), instrument("B")],
        current_weights: BTreeMap::new(),
        expected_returns,
        covariance,
    }
}

/// Diagonal covariance `diag(0.04, 0.16)` → volatilities 0.2 and 0.4, so risk
/// parity targets inverse-vol weights `[2/3, 1/3]`.
fn diagonal_covariance_state() -> PortfolioState {
    let mut covariance = BTreeMap::new();
    let mut row_a = BTreeMap::new();
    row_a.insert(instrument("A"), d("0.04"));
    row_a.insert(instrument("B"), d("0"));
    let mut row_b = BTreeMap::new();
    row_b.insert(instrument("A"), d("0"));
    row_b.insert(instrument("B"), d("0.16"));
    covariance.insert(instrument("A"), row_a);
    covariance.insert(instrument("B"), row_b);

    PortfolioState {
        as_of: now(),
        instruments: vec![instrument("A"), instrument("B")],
        current_weights: BTreeMap::new(),
        expected_returns: BTreeMap::new(),
        covariance,
    }
}

fn weights(target: &TargetWeights) -> BTreeMap<String, Decimal> {
    target
        .weights
        .iter()
        .map(|(id, w)| (id.symbol.clone(), *w))
        .collect()
}

/// `&str` → `String`, for indexing the `BTreeMap<String, Decimal>` from
/// [`weights`] without inference ambiguity.
fn sym(s: &str) -> String {
    s.to_string()
}

fn assert_sums_to_one(target: &TargetWeights) {
    let sum: Decimal = target.weights.values().copied().sum();
    assert!(
        (sum - Decimal::ONE).abs() < d("0.000001"),
        "weights sum to {sum}, expected 1"
    );
}

// ---------------------------------------------------------------------------
// Mean-Variance
// ---------------------------------------------------------------------------

#[test]
fn mean_variance_tangency_golden() {
    let objective = AllocationObjective::MeanVariance {
        risk_free_rate: d("0.05"),
    };
    let target = optimize(objective, &[], &two_asset_state()).unwrap();

    // Σ⁻¹(μ − r_f) = [0.02, 0.06], normalized to [0.25, 0.75].
    let w = weights(&target);
    assert_eq!(w[&sym("A")], d("0.25"));
    assert_eq!(w[&sym("B")], d("0.75"));
    assert_sums_to_one(&target);
    assert_eq!(target.as_of, now());
}

#[test]
fn mean_variance_respects_weight_bounds() {
    let objective = AllocationObjective::MeanVariance {
        risk_free_rate: d("0.05"),
    };
    let constraints = [Constraint::WeightBounds {
        instrument: instrument("B"),
        min: d("0"),
        max: d("0.5"),
    }];
    let target = optimize(objective, &constraints, &two_asset_state()).unwrap();

    // Tangency [0.25, 0.75] clips B to 0.5; A renormalizes to 0.5.
    let w = weights(&target);
    assert_eq!(w[&sym("A")], d("0.5"));
    assert_eq!(w[&sym("B")], d("0.5"));
    assert_sums_to_one(&target);
}

#[test]
fn mean_variance_requires_expected_returns() {
    let mut state = two_asset_state();
    state.expected_returns = BTreeMap::new();
    let err = optimize(
        AllocationObjective::MeanVariance {
            risk_free_rate: d("0.05"),
        },
        &[],
        &state,
    )
    .unwrap_err();
    assert!(
        matches!(err, AllocationError::InvalidArgument(msg) if msg.contains("missing expected return"))
    );
}

#[test]
fn mean_variance_singular_covariance_is_an_error() {
    let mut state = two_asset_state();
    let mut covariance = BTreeMap::new();
    let mut row_a = BTreeMap::new();
    row_a.insert(instrument("A"), d("1"));
    row_a.insert(instrument("B"), d("2"));
    let mut row_b = BTreeMap::new();
    row_b.insert(instrument("A"), d("2"));
    row_b.insert(instrument("B"), d("4"));
    covariance.insert(instrument("A"), row_a);
    covariance.insert(instrument("B"), row_b);
    state.covariance = covariance;

    assert_eq!(
        optimize(
            AllocationObjective::MeanVariance {
                risk_free_rate: d("0.05"),
            },
            &[],
            &state,
        )
        .unwrap_err(),
        AllocationError::SingularCovariance
    );
}

// ---------------------------------------------------------------------------
// Risk Parity
// ---------------------------------------------------------------------------

#[test]
fn risk_parity_diagonal_covariance_is_inverse_volatility() {
    let target = optimize(
        AllocationObjective::RiskParity,
        &[],
        &diagonal_covariance_state(),
    )
    .unwrap();

    // w ∝ [1/0.2, 1/0.4] = [5, 2.5] → [2/3, 1/3].
    let w = weights(&target);
    assert_eq!(w[&sym("A")].round_dp(6), d("0.666667"));
    assert_eq!(w[&sym("B")].round_dp(6), d("0.333333"));
    assert_sums_to_one(&target);
}

#[test]
fn risk_parity_respects_weight_bounds() {
    let constraints = [Constraint::WeightBounds {
        instrument: instrument("A"),
        min: d("0"),
        max: d("0.5"),
    }];
    let target = optimize(
        AllocationObjective::RiskParity,
        &constraints,
        &diagonal_covariance_state(),
    )
    .unwrap();
    let w = weights(&target);
    assert!(w[&sym("A")] <= d("0.5"));
    assert_sums_to_one(&target);
}

#[test]
fn risk_parity_singular_covariance_is_an_error() {
    // PSD singular matrix: under equal weights the marginal risk vector is
    // all zeros, so the risk-parity reciprocal blows up.
    let mut state = diagonal_covariance_state();
    state.covariance = {
        let mut covariance = BTreeMap::new();
        let mut row_a = BTreeMap::new();
        row_a.insert(instrument("A"), d("1"));
        row_a.insert(instrument("B"), d("-1"));
        let mut row_b = BTreeMap::new();
        row_b.insert(instrument("A"), d("-1"));
        row_b.insert(instrument("B"), d("1"));
        covariance.insert(instrument("A"), row_a);
        covariance.insert(instrument("B"), row_b);
        covariance
    };
    assert!(matches!(
        optimize(AllocationObjective::RiskParity, &[], &state),
        Err(AllocationError::SingularCovariance)
    ));
}

// ---------------------------------------------------------------------------
// Constraint validation
// ---------------------------------------------------------------------------

#[test]
fn min_greater_than_max_is_infeasible() {
    let constraints = [Constraint::WeightBounds {
        instrument: instrument("A"),
        min: d("0.6"),
        max: d("0.5"),
    }];
    let err = optimize(
        AllocationObjective::RiskParity,
        &constraints,
        &diagonal_covariance_state(),
    )
    .unwrap_err();
    assert!(
        matches!(err, AllocationError::InfeasibleConstraints(msg) if msg.contains("min 0.6 > max 0.5"))
    );
}

#[test]
fn sum_of_mins_above_one_is_infeasible() {
    let constraints = [
        Constraint::WeightBounds {
            instrument: instrument("A"),
            min: d("0.6"),
            max: d("1"),
        },
        Constraint::WeightBounds {
            instrument: instrument("B"),
            min: d("0.6"),
            max: d("1"),
        },
    ];
    let err = optimize(
        AllocationObjective::RiskParity,
        &constraints,
        &diagonal_covariance_state(),
    )
    .unwrap_err();
    assert!(
        matches!(err, AllocationError::InfeasibleConstraints(msg) if msg.contains("sum of minimum weight bounds exceeds 1"))
    );
}

#[test]
fn sum_of_maxes_below_one_is_infeasible() {
    let constraints = [
        Constraint::WeightBounds {
            instrument: instrument("A"),
            min: d("0"),
            max: d("0.4"),
        },
        Constraint::WeightBounds {
            instrument: instrument("B"),
            min: d("0"),
            max: d("0.4"),
        },
    ];
    let err = optimize(
        AllocationObjective::RiskParity,
        &constraints,
        &diagonal_covariance_state(),
    )
    .unwrap_err();
    assert!(
        matches!(err, AllocationError::InfeasibleConstraints(msg) if msg.contains("sum of maximum weight bounds is below 1"))
    );
}

#[test]
fn constraint_for_unknown_instrument_is_an_error() {
    let constraints = [Constraint::WeightBounds {
        instrument: instrument("C"),
        min: d("0"),
        max: d("1"),
    }];
    let err = optimize(
        AllocationObjective::RiskParity,
        &constraints,
        &diagonal_covariance_state(),
    )
    .unwrap_err();
    assert!(
        matches!(err, AllocationError::InvalidArgument(msg) if msg.contains("not in the universe"))
    );
}

// ---------------------------------------------------------------------------
// HRP (plan 0003 / gap 0001)
// ---------------------------------------------------------------------------

#[test]
fn hrp_diagonal_covariance_is_inverse_variance() {
    // Uncorrelated HRP reduces to inverse-variance weights:
    // [1/0.04, 1/0.16] ∝ [25, 6.25] → [0.8, 0.2].
    let target = optimize(AllocationObjective::Hrp, &[], &diagonal_covariance_state()).unwrap();
    let w = weights(&target);
    assert_eq!(w[&sym("A")].round_dp(6), d("0.8"));
    assert_eq!(w[&sym("B")].round_dp(6), d("0.2"));
    assert_sums_to_one(&target);
}

#[test]
fn hrp_three_asset_diagonal_golden() {
    // diag(0.04, 0.16, 0.25): HRP ≡ inverse-variance
    // [25/35.25, 6.25/35.25, 4/35.25] = [0.709220, 0.177305, 0.113475].
    let mut state = three_asset_identity_state();
    state.covariance = {
        let mut cov = BTreeMap::new();
        let diag = [d("0.04"), d("0.16"), d("0.25")];
        for (i, id) in state.instruments.iter().enumerate() {
            let mut row = BTreeMap::new();
            for (j, other) in state.instruments.iter().enumerate() {
                row.insert(other.clone(), if i == j { diag[i] } else { Decimal::ZERO });
            }
            cov.insert(id.clone(), row);
        }
        cov
    };
    let target = optimize(AllocationObjective::Hrp, &[], &state).unwrap();
    let w = weights(&target);
    assert_eq!(w[&sym("A")].round_dp(6), d("0.709220"));
    assert_eq!(w[&sym("B")].round_dp(6), d("0.177305"));
    assert_eq!(w[&sym("C")].round_dp(6), d("0.113475"));
    assert_sums_to_one(&target);
}

#[test]
fn hrp_two_asset_correlated_is_inverse_variance() {
    // With two assets the single bisection is inverse-variance regardless of
    // correlation: [[1, 0.5], [0.5, 1]] → [0.5, 0.5].
    let target = optimize(AllocationObjective::Hrp, &[], &two_asset_state()).unwrap();
    let w = weights(&target);
    assert_eq!(w[&sym("A")].round_dp(6), d("0.5"));
    assert_eq!(w[&sym("B")].round_dp(6), d("0.5"));
    assert_sums_to_one(&target);
}

#[test]
fn hrp_respects_weight_bounds() {
    // Projection onto the bounded simplex must keep every weight in bounds.
    let constraints = [Constraint::WeightBounds {
        instrument: instrument("A"),
        min: d("0"),
        max: d("0.5"),
    }];
    let target = optimize(AllocationObjective::Hrp, &constraints, &two_asset_state()).unwrap();
    let w = weights(&target);
    assert!(w[&sym("A")] <= d("0.5"));
    assert_sums_to_one(&target);
}

// ---------------------------------------------------------------------------
// Black-Litterman (plan 0003 / gap 0002)
// ---------------------------------------------------------------------------

fn bl_views(market: &[(InstrumentId, Decimal)], views: Vec<View>) -> BlackLittermanViews {
    let mut market_weights = BTreeMap::new();
    for (id, w) in market {
        market_weights.insert(id.clone(), *w);
    }
    BlackLittermanViews {
        tau: d("0.1"),
        market_weights,
        views,
    }
}

#[test]
fn black_litterman_no_views_returns_market_weights() {
    // With no views the posterior collapses to the market-implied prior and
    // the allocation returns the benchmark weights unchanged.
    let objective = AllocationObjective::BlackLitterman {
        views: bl_views(
            &[(instrument("A"), d("0.6")), (instrument("B"), d("0.4"))],
            vec![],
        ),
    };
    let target = optimize(objective, &[], &two_asset_state()).unwrap();
    let w = weights(&target);
    assert_eq!(w[&sym("A")].round_dp(6), d("0.6"));
    assert_eq!(w[&sym("B")].round_dp(6), d("0.4"));
    assert_sums_to_one(&target);
}

#[test]
fn black_litterman_view_blend_golden() {
    // Absolute view "A will return 10%" with tight confidence (σ = 0.05,
    // Ω⁻¹ = 400). rhs = w_mkt + PᵀΩ⁻¹q = [0.6, 0.4] + [40, 0] = [40.6, 0.4],
    // so the posterior tangency is [203/205, 2/205] ≈ [0.990244, 0.009756].
    let objective = AllocationObjective::BlackLitterman {
        views: bl_views(
            &[(instrument("A"), d("0.6")), (instrument("B"), d("0.4"))],
            vec![View {
                instruments: vec![instrument("A")],
                coefficients: vec![Decimal::ONE],
                mean: d("0.10"),
                uncertainty: d("0.05"),
            }],
        ),
    };
    let target = optimize(objective, &[], &two_asset_state()).unwrap();
    let w = weights(&target);
    assert_eq!(w[&sym("A")].round_dp(6), d("0.990244"));
    assert_eq!(w[&sym("B")].round_dp(6), d("0.009756"));
    assert_sums_to_one(&target);
}

#[test]
fn black_litterman_rejects_bad_inputs() {
    // tau must be positive.
    let mut views = bl_views(
        &[(instrument("A"), d("0.6")), (instrument("B"), d("0.4"))],
        vec![],
    );
    views.tau = Decimal::ZERO;
    assert!(matches!(
        optimize(
            AllocationObjective::BlackLitterman { views },
            &[],
            &two_asset_state(),
        ),
        Err(AllocationError::InvalidArgument(msg)) if msg.contains("tau")
    ));

    // Market weights must cover the whole universe.
    let views = bl_views(&[(instrument("A"), d("1"))], vec![]);
    assert!(matches!(
        optimize(
            AllocationObjective::BlackLitterman { views },
            &[],
            &two_asset_state(),
        ),
        Err(AllocationError::InvalidArgument(msg)) if msg.contains("missing market weight")
    ));
}

// ---------------------------------------------------------------------------
// Exact bound-constrained solve (plan 0003 / gap 0003)
// ---------------------------------------------------------------------------

/// Identity-covariance 3-asset universe with expected returns
/// `[0.20, 0.10, 0.05]` (no risk-free rate).
fn three_asset_identity_state() -> PortfolioState {
    let mut expected_returns = BTreeMap::new();
    expected_returns.insert(instrument("A"), d("0.20"));
    expected_returns.insert(instrument("B"), d("0.10"));
    expected_returns.insert(instrument("C"), d("0.05"));

    let mut covariance = BTreeMap::new();
    for (i, id) in [instrument("A"), instrument("B"), instrument("C")]
        .iter()
        .enumerate()
    {
        let mut row = BTreeMap::new();
        for (j, other) in [instrument("A"), instrument("B"), instrument("C")]
            .iter()
            .enumerate()
        {
            row.insert(
                other.clone(),
                if i == j { Decimal::ONE } else { Decimal::ZERO },
            );
        }
        covariance.insert(id.clone(), row);
    }

    PortfolioState {
        as_of: now(),
        instruments: vec![instrument("A"), instrument("B"), instrument("C")],
        current_weights: BTreeMap::new(),
        expected_returns,
        covariance,
    }
}

#[test]
fn mean_variance_exact_box_solver_golden() {
    // Unconstrained tangency (identity Σ) is [0.571428, 0.285714, 0.142857];
    // B exceeds its 0.2 cap. The box-constrained tangency pins B at 0.2 and
    // solves the reduced 1-D problem to [0.625, 0.2, 0.175]. The Phase-1
    // clip-and-renormalize heuristic produced [0.64, 0.2, 0.16] instead —
    // the exact solver is what the golden pins.
    let constraints = [
        Constraint::WeightBounds {
            instrument: instrument("B"),
            min: d("0"),
            max: d("0.2"),
        },
        Constraint::WeightBounds {
            instrument: instrument("C"),
            min: d("0"),
            max: d("0.2"),
        },
    ];
    let target = optimize(
        AllocationObjective::MeanVariance {
            risk_free_rate: Decimal::ZERO,
        },
        &constraints,
        &three_asset_identity_state(),
    )
    .unwrap();
    let w = weights(&target);
    assert_eq!(w[&sym("A")].round_dp(6), d("0.625"));
    assert_eq!(w[&sym("B")].round_dp(6), d("0.2"));
    assert_eq!(w[&sym("C")].round_dp(6), d("0.175"));
    assert_sums_to_one(&target);
}

#[test]
fn mean_variance_exact_solver_keeps_unconstrained_golden_when_box_inactive() {
    // When the tangency is inside the box the exact solution must equal the
    // Phase-1 unconstrained golden [0.25, 0.75] exactly.
    let target = optimize(
        AllocationObjective::MeanVariance {
            risk_free_rate: d("0.05"),
        },
        &[],
        &two_asset_state(),
    )
    .unwrap();
    let w = weights(&target);
    assert_eq!(w[&sym("A")], d("0.25"));
    assert_eq!(w[&sym("B")], d("0.75"));
}

#[test]
fn risk_parity_exact_projection_respects_bounds() {
    // diag(0.04, 0.16): unconstrained RP is [2/3, 1/3]; with A capped at 0.5
    // the projected fixed point pins A at its bound and rebalances B.
    let constraints = [Constraint::WeightBounds {
        instrument: instrument("A"),
        min: d("0"),
        max: d("0.5"),
    }];
    let target = optimize(
        AllocationObjective::RiskParity,
        &constraints,
        &diagonal_covariance_state(),
    )
    .unwrap();
    let w = weights(&target);
    assert!(w[&sym("A")] <= d("0.5"));
    assert_sums_to_one(&target);
}

// ---------------------------------------------------------------------------
// Discrete / whole-share allocation (plan 0003 / gap 0004)
// ---------------------------------------------------------------------------

fn discrete_state() -> PortfolioState {
    let mut expected_returns = BTreeMap::new();
    expected_returns.insert(instrument("A"), d("0.10"));
    expected_returns.insert(instrument("B"), d("0.10"));
    let mut covariance = BTreeMap::new();
    let mut row_a = BTreeMap::new();
    row_a.insert(instrument("A"), d("1"));
    row_a.insert(instrument("B"), d("0"));
    let mut row_b = BTreeMap::new();
    row_b.insert(instrument("A"), d("0"));
    row_b.insert(instrument("B"), d("1"));
    covariance.insert(instrument("A"), row_a);
    covariance.insert(instrument("B"), row_b);
    PortfolioState {
        as_of: now(),
        instruments: vec![instrument("A"), instrument("B")],
        current_weights: BTreeMap::new(),
        expected_returns,
        covariance,
    }
}

#[test]
fn allocate_discrete_exact_whole_shares() {
    let target = optimize(AllocationObjective::RiskParity, &[], &discrete_state()).unwrap();
    let mut prices = BTreeMap::new();
    prices.insert(instrument("A"), Price::from_rupees("100").unwrap());
    prices.insert(instrument("B"), Price::from_rupees("100").unwrap());
    let quantities =
        allocate_discrete(&target, Money::inr(d("10000")), &prices, &BTreeMap::new()).unwrap();
    // [0.5, 0.5] × 10,000 / 100 → exactly 50 shares each.
    assert_eq!(quantities[&instrument("A")], Quantity::from_u64(50));
    assert_eq!(quantities[&instrument("B")], Quantity::from_u64(50));
}

#[test]
fn allocate_discrete_largest_remainder() {
    // Equal thirds over 10,000 at 100/share: two of the three instruments get
    // 34 shares (the fractional tie breaks by instrument order), one gets 33.
    let state = three_asset_identity_state();
    let target = optimize(AllocationObjective::RiskParity, &[], &state).unwrap();
    let mut prices = BTreeMap::new();
    prices.insert(instrument("A"), Price::from_rupees("100").unwrap());
    prices.insert(instrument("B"), Price::from_rupees("100").unwrap());
    prices.insert(instrument("C"), Price::from_rupees("100").unwrap());
    let quantities =
        allocate_discrete(&target, Money::inr(d("10000")), &prices, &BTreeMap::new()).unwrap();
    // RiskParity on identity covariance → equal thirds.
    assert_eq!(quantities[&instrument("A")], Quantity::from_u64(34));
    assert_eq!(quantities[&instrument("B")], Quantity::from_u64(33));
    assert_eq!(quantities[&instrument("C")], Quantity::from_u64(33));
}

#[test]
fn allocate_discrete_respects_lot_sizes() {
    let target = optimize(AllocationObjective::RiskParity, &[], &discrete_state()).unwrap();
    let mut prices = BTreeMap::new();
    prices.insert(instrument("A"), Price::from_rupees("100").unwrap());
    prices.insert(instrument("B"), Price::from_rupees("100").unwrap());
    let mut lots = BTreeMap::new();
    lots.insert(instrument("A"), Quantity::from_u64(25));
    lots.insert(instrument("B"), Quantity::from_u64(10));
    let quantities = allocate_discrete(&target, Money::inr(d("10000")), &prices, &lots).unwrap();
    // A: 5000 / (100·25) = 2 lots → 50 shares; B: 5000 / (100·10) = 5 lots → 50.
    assert_eq!(quantities[&instrument("A")], Quantity::from_u64(50));
    assert_eq!(quantities[&instrument("B")], Quantity::from_u64(50));
    // Both are exact lot multiples.
    assert!(quantities[&instrument("A")].is_lot_multiple(25));
    assert!(quantities[&instrument("B")].is_lot_multiple(10));
}

#[test]
fn allocate_discrete_keeps_residual_as_cash() {
    // 300/share × two instruments: 16 whole shares each (9600), residual 400
    // buys one extra share of the first instrument → [17, 16], 100 left as
    // cash (within one lot of the target equity).
    let target = optimize(AllocationObjective::RiskParity, &[], &discrete_state()).unwrap();
    let mut prices = BTreeMap::new();
    prices.insert(instrument("A"), Price::from_rupees("300").unwrap());
    prices.insert(instrument("B"), Price::from_rupees("300").unwrap());
    let quantities =
        allocate_discrete(&target, Money::inr(d("10000")), &prices, &BTreeMap::new()).unwrap();
    assert_eq!(quantities[&instrument("A")], Quantity::from_u64(17));
    assert_eq!(quantities[&instrument("B")], Quantity::from_u64(16));
}

#[test]
fn allocate_discrete_rejects_weights_not_summing_to_one() {
    let target = optimize(AllocationObjective::RiskParity, &[], &discrete_state()).unwrap();
    let mut weights = target.weights.clone();
    // Corrupt one weight so the map no longer sums to 1.
    weights.insert(instrument("A"), d("0.9"));
    let target = TargetWeights {
        as_of: target.as_of,
        objective: target.objective,
        weights,
    };
    let mut prices = BTreeMap::new();
    prices.insert(instrument("A"), Price::from_rupees("100").unwrap());
    prices.insert(instrument("B"), Price::from_rupees("100").unwrap());
    assert!(matches!(
        allocate_discrete(&target, Money::inr(d("10000")), &prices, &BTreeMap::new()),
        Err(AllocationError::InvalidArgument(msg)) if msg.contains("sum to 1")
    ));
}

// ---------------------------------------------------------------------------
// Not-yet-implemented objectives and input validation
// ---------------------------------------------------------------------------

#[test]
fn empty_universe_is_an_error() {
    let state = PortfolioState {
        as_of: now(),
        instruments: vec![],
        current_weights: BTreeMap::new(),
        expected_returns: BTreeMap::new(),
        covariance: BTreeMap::new(),
    };
    assert_eq!(
        optimize(AllocationObjective::RiskParity, &[], &state).unwrap_err(),
        AllocationError::InvalidArgument("instrument universe is empty".into())
    );
}

#[test]
fn missing_covariance_row_is_an_error() {
    let mut state = two_asset_state();
    state.covariance = BTreeMap::new();
    let err = optimize(AllocationObjective::RiskParity, &[], &state).unwrap_err();
    assert!(
        matches!(err, AllocationError::InvalidArgument(msg) if msg.contains("missing covariance row"))
    );
}

// ---------------------------------------------------------------------------
// Read-adapter over tantra-ledger-portfolio
// ---------------------------------------------------------------------------

#[test]
fn from_portfolio_computes_current_weights_and_optimizes() {
    let instr_a = instrument("A");
    let instr_b = instrument("B");

    let mut portfolio = tantra_ledger_portfolio::PortfolioState::new(Currency::Inr);
    let account = AccountState::new(
        Currency::Inr,
        vec![
            HeldPosition::new(
                instr_a.clone(),
                PositionSide::Long,
                Quantity::from_u64(100),
                Price::from_rupees("10").unwrap(),
            ),
            HeldPosition::new(
                instr_b.clone(),
                PositionSide::Long,
                Quantity::from_u64(30),
                Price::from_rupees("10").unwrap(),
            ),
        ],
    );
    portfolio.accounts.insert("acc-1".into(), account);

    let marks = Marks::new()
        .with(instr_a.clone(), Price::from_rupees("10").unwrap())
        .with(instr_b.clone(), Price::from_rupees("10").unwrap());

    let state = PortfolioState::from_portfolio(&portfolio, &marks, now());
    assert_eq!(state.instruments.len(), 2);
    assert_eq!(
        state.current_weights[&instr_a].round_dp(6),
        d("0.769231") // 1000 / 1300
    );
    assert_eq!(
        state.current_weights[&instr_b].round_dp(6),
        d("0.230769") // 300 / 1300
    );

    // Attach the same golden inputs as the other tests and optimize.
    let mut expected_returns = BTreeMap::new();
    expected_returns.insert(instr_a.clone(), d("0.10"));
    expected_returns.insert(instr_b.clone(), d("0.12"));
    let mut covariance = BTreeMap::new();
    let mut row_a = BTreeMap::new();
    row_a.insert(instr_a.clone(), d("1"));
    row_a.insert(instr_b.clone(), d("0.5"));
    let mut row_b = BTreeMap::new();
    row_b.insert(instr_a.clone(), d("0.5"));
    row_b.insert(instr_b.clone(), d("1"));
    covariance.insert(instr_a.clone(), row_a);
    covariance.insert(instr_b.clone(), row_b);

    let state = state
        .with_expected_returns(expected_returns)
        .with_covariance(covariance);
    let target = optimize(
        AllocationObjective::MeanVariance {
            risk_free_rate: d("0.05"),
        },
        &[],
        &state,
    )
    .unwrap();
    assert_eq!(target.weights[&instr_a], d("0.25"));
    assert_eq!(target.weights[&instr_b], d("0.75"));
}

// ---------------------------------------------------------------------------
// Serde / auditability
// ---------------------------------------------------------------------------

#[test]
fn target_weights_serde_roundtrip() {
    let target = optimize(
        AllocationObjective::RiskParity,
        &[],
        &diagonal_covariance_state(),
    )
    .unwrap();
    let json = serde_json::to_string(&target).unwrap();
    let back: TargetWeights = serde_json::from_str(&json).unwrap();
    assert_eq!(target, back);
    assert_eq!(back.objective, AllocationObjective::RiskParity);
}

#[test]
fn objective_and_constraint_serde_roundtrip() {
    let objective = AllocationObjective::MeanVariance {
        risk_free_rate: d("0.05"),
    };
    let json = serde_json::to_string(&objective).unwrap();
    let back: AllocationObjective = serde_json::from_str(&json).unwrap();
    assert_eq!(objective, back);

    let constraint = Constraint::WeightBounds {
        instrument: instrument("A"),
        min: d("0"),
        max: d("1"),
    };
    let json = serde_json::to_string(&constraint).unwrap();
    let back: Constraint = serde_json::from_str(&json).unwrap();
    assert_eq!(constraint, back);
}

#[test]
fn black_litterman_objective_serde_roundtrip() {
    let objective = AllocationObjective::BlackLitterman {
        views: BlackLittermanViews {
            tau: d("0.1"),
            market_weights: {
                let mut m = BTreeMap::new();
                m.insert(instrument("A"), d("0.6"));
                m.insert(instrument("B"), d("0.4"));
                m
            },
            views: vec![View {
                instruments: vec![instrument("A")],
                coefficients: vec![Decimal::ONE],
                mean: d("0.10"),
                uncertainty: d("0.05"),
            }],
        },
    };
    let json = serde_json::to_string(&objective).unwrap();
    let back: AllocationObjective = serde_json::from_str(&json).unwrap();
    assert_eq!(objective, back);
    // The internally-tagged wire shape carries the views payload.
    assert!(json.contains("\"type\":\"black_litterman\""));
    assert!(json.contains("views"));
}
