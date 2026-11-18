//! Integration tests for `tantra_ledger_allocation::fire_buckets` (`T19.1` / plan 0006).

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use tantra_core::{Currency, Exchange, InstrumentId, Money, Price, Quantity};
use tantra_ledger_allocation::{
    AllocationObjective, CashflowBucketConfig, FireBucketKind, FireError, FireGlidepathPolicy,
    TargetWeights, allocate_fire_bucket_discrete, compute_bucket_drift,
};

fn as_of() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 18, 9, 0, 0)
        .single()
        .unwrap()
}

fn inst(sym: &str) -> InstrumentId {
    InstrumentId::new(Exchange::Nse, sym)
}

#[test]
fn glidepath_endpoints_and_midpoint_interpolation_sum_to_100() {
    let policy = FireGlidepathPolicy::default(); // 85/10/5 accumulation -> 65/25/10 retirement over 10y

    // >= 10 years before FIRE: exact accumulation weights.
    let far = policy
        .target_mix_at_years_to_fire(Decimal::from(15))
        .unwrap();
    assert_eq!(far.equity_pct, Decimal::from(85));
    assert_eq!(far.debt_pct, Decimal::from(10));
    assert_eq!(far.theta_margin_pct, Decimal::from(5));

    // <= 0 years to FIRE: exact retirement mix.
    let retired = policy.target_mix_at_years_to_fire(Decimal::ZERO).unwrap();
    assert_eq!(retired.equity_pct, Decimal::from(65));
    assert_eq!(retired.debt_pct, Decimal::from(25));
    assert_eq!(retired.theta_margin_pct, Decimal::from(10));

    // Midpoint (5 years remaining): exact 75 / 17.5 / 7.5 = 100.
    let mid = policy
        .target_mix_at_years_to_fire(Decimal::from(5))
        .unwrap();
    assert_eq!(mid.equity_pct, Decimal::from(75));
    assert_eq!(mid.debt_pct, Decimal::new(175, 1));
    assert_eq!(mid.theta_margin_pct, Decimal::new(75, 1));
    assert_eq!(
        mid.equity_pct + mid.debt_pct + mid.theta_margin_pct,
        Decimal::from(100)
    );
}

#[test]
fn compute_bucket_drift_calculates_exact_rebalance_deltas() {
    let target = CashflowBucketConfig::default(); // 65% eq, 25% debt, 10% theta
    let mut held = BTreeMap::new();
    // Total = ₹1,00,00,000 (75% eq, 15% debt, 10% theta)
    held.insert(
        FireBucketKind::GrowthEquity,
        Money::inr(Decimal::from(7_500_000)),
    );
    held.insert(
        FireBucketKind::LiquidityRunway,
        Money::inr(Decimal::from(1_500_000)),
    );
    held.insert(
        FireBucketKind::ThetaMargin,
        Money::inr(Decimal::from(1_000_000)),
    );

    let drift = compute_bucket_drift(&held, &target, Currency::Inr, as_of()).unwrap();
    assert_eq!(
        drift.total_portfolio_value,
        Money::inr(Decimal::from(10_000_000))
    );
    assert_eq!(drift.buckets.len(), 3);

    let eq = &drift.buckets[0];
    assert_eq!(eq.bucket, FireBucketKind::GrowthEquity);
    assert_eq!(eq.drift_pct, Decimal::from(10)); // +10% overweight
    assert_eq!(eq.rebalance_delta, Money::inr(Decimal::from(-1_000_000))); // trim ₹10L

    let debt = &drift.buckets[1];
    assert_eq!(debt.bucket, FireBucketKind::LiquidityRunway);
    assert_eq!(debt.drift_pct, Decimal::from(-10)); // -10% underweight
    assert_eq!(debt.rebalance_delta, Money::inr(Decimal::from(1_000_000))); // add ₹10L

    let theta = &drift.buckets[2];
    assert_eq!(theta.bucket, FireBucketKind::ThetaMargin);
    assert_eq!(theta.drift_pct, Decimal::ZERO);
    assert_eq!(theta.rebalance_delta, Money::inr(Decimal::ZERO));
}

#[test]
fn allocate_fire_bucket_discrete_sizes_whole_lots_within_sleeve_budget() {
    let tcs = inst("TCS");
    let infy = inst("INFY");

    let mut weights = BTreeMap::new();
    weights.insert(tcs.clone(), Decimal::new(6, 1)); // 60%
    weights.insert(infy.clone(), Decimal::new(4, 1)); // 40%
    let target = TargetWeights {
        as_of: as_of(),
        objective: AllocationObjective::RiskParity,
        weights,
    };

    let mut prices = BTreeMap::new();
    prices.insert(tcs.clone(), Price::new(Decimal::from(4_000)));
    prices.insert(infy.clone(), Price::new(Decimal::from(1_500)));

    let mut lot_sizes = BTreeMap::new();
    lot_sizes.insert(tcs.clone(), Quantity::from_u64(1));
    lot_sizes.insert(infy.clone(), Quantity::from_u64(1));

    // ₹10,00,000 total corpus * 65% GrowthEquity sleeve = ₹6,50,000 budget
    let qty = allocate_fire_bucket_discrete(
        Money::inr(Decimal::from(1_000_000)),
        Decimal::from(65),
        &target,
        &prices,
        &lot_sizes,
    )
    .unwrap();

    let spent =
        qty[&tcs].value() * Decimal::from(4_000) + qty[&infy].value() * Decimal::from(1_500);
    assert!(spent <= Decimal::from(650_000));
    assert!(spent > Decimal::from(645_000));
    assert_eq!(qty[&tcs].value().fract(), Decimal::ZERO);
    assert_eq!(qty[&infy].value().fract(), Decimal::ZERO);
}

#[test]
fn glidepath_and_drift_reject_currency_mismatch_and_bad_weights() {
    let bad_policy = FireGlidepathPolicy {
        accumulation_equity_pct: Decimal::from(99),
        ..Default::default()
    };
    assert!(matches!(
        bad_policy.validate(),
        Err(FireError::InvalidInput(_))
    ));

    let mut held = BTreeMap::new();
    held.insert(
        FireBucketKind::GrowthEquity,
        Money::new(Decimal::from(1_000), Currency::Usd),
    );
    assert!(matches!(
        compute_bucket_drift(
            &held,
            &CashflowBucketConfig::default(),
            Currency::Inr,
            as_of()
        ),
        Err(FireError::CurrencyMismatch {
            expected: Currency::Inr,
            found: Currency::Usd
        })
    ));
}
