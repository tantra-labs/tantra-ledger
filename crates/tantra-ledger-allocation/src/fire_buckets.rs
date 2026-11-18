//! 3-Bucket FIRE Glidepath Allocation & Drift Engine (`tantra-ledger` plan 0006).
//!
//! Bridges the static 3-bucket post-FIRE paycheck (`GrowthEquity`,
//! `LiquidityRunway`, `ThetaMargin`) from [`crate::fire`] into live portfolio
//! target-weight interpolation and [`crate::allocate_discrete`] lot rebalancing.
//! Pure `Decimal`, zero I/O, zero clocks (`as_of` supplied explicitly).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use serde::{Deserialize, Serialize};

use crate::fire::{CashflowBucketConfig, FireError};
use crate::{
    AllocationError, Currency, InstrumentId, Money, Price, Quantity, TargetWeights,
    allocate_discrete,
};

const HUNDRED: u64 = 100;

/// Role of an instrument or capital sleeve inside a FIRE 3-bucket portfolio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FireBucketKind {
    /// Core direct equities / index momentum wealth compounder.
    GrowthEquity,
    /// Liquid debt, arbitrage funds, or overnight cash buffer (3–4y runway).
    LiquidityRunway,
    /// Options collateral / margin sleeve harvesting systematic theta yield.
    ThetaMargin,
}

/// Tag associating a portfolio instrument with a FIRE bucket sleeve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketClassifiedInstrument {
    pub instrument: InstrumentId,
    pub bucket: FireBucketKind,
}

/// Linear-glidepath policy transitioning from accumulation weights into the
/// post-FIRE retirement paycheck bucket mix as `years_to_fire → 0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireGlidepathPolicy {
    pub accumulation_equity_pct: Decimal,
    pub accumulation_debt_pct: Decimal,
    pub accumulation_theta_pct: Decimal,
    /// Years before FIRE over which weights glide linearly toward `retirement_mix`.
    pub transition_years: u16,
    pub retirement_mix: CashflowBucketConfig,
}

impl Default for FireGlidepathPolicy {
    fn default() -> Self {
        Self {
            accumulation_equity_pct: Decimal::from(85),
            accumulation_debt_pct: Decimal::from(10),
            accumulation_theta_pct: Decimal::from(5),
            transition_years: 10,
            retirement_mix: CashflowBucketConfig::default(),
        }
    }
}

impl FireGlidepathPolicy {
    /// Validates both endpoints (`accumulation_*` and `retirement_mix`) sum to 100%.
    pub fn validate(&self) -> Result<(), FireError> {
        let hundred = Decimal::from(HUNDRED);
        let zero = Decimal::ZERO;
        let tol = Decimal::new(1, 4);
        if self.accumulation_equity_pct < zero
            || self.accumulation_debt_pct < zero
            || self.accumulation_theta_pct < zero
        {
            return Err(FireError::InvalidInput(
                "accumulation bucket percentages cannot be negative".into(),
            ));
        }
        let acc_sum =
            self.accumulation_equity_pct + self.accumulation_debt_pct + self.accumulation_theta_pct;
        if (acc_sum - hundred).abs() > tol {
            return Err(FireError::InvalidInput(format!(
                "accumulation bucket weights must sum to 100 (got {acc_sum})"
            )));
        }
        self.retirement_mix.validate()?;
        Ok(())
    }

    /// Interpolates exact bucket percentages for the given remaining `years_to_fire`.
    ///
    /// - When `years_to_fire >= transition_years` (or `transition_years == 0` with positive years):
    ///   returns the accumulation endpoint (or `retirement_mix` if `years_to_fire <= 0`).
    /// - Sums exact blended weights cleanly to `100.00`%.
    pub fn target_mix_at_years_to_fire(
        &self,
        years_to_fire: Decimal,
    ) -> Result<CashflowBucketConfig, FireError> {
        self.validate()?;
        if years_to_fire <= Decimal::ZERO {
            return Ok(self.retirement_mix.clone());
        }
        if self.transition_years == 0 || years_to_fire >= Decimal::from(self.transition_years) {
            return Ok(CashflowBucketConfig {
                equity_pct: self.accumulation_equity_pct,
                debt_pct: self.accumulation_debt_pct,
                theta_margin_pct: self.accumulation_theta_pct,
                ..self.retirement_mix.clone()
            });
        }

        // alpha = 0 at FIRE (years_to_fire = 0), 1 at transition_years
        let alpha = (years_to_fire / Decimal::from(self.transition_years))
            .clamp(Decimal::ZERO, Decimal::ONE);
        let one_minus = Decimal::ONE - alpha;

        let blend = |acc: Decimal, ret: Decimal| -> Decimal {
            (acc * alpha + ret * one_minus)
                .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
        };

        let eq = blend(self.accumulation_equity_pct, self.retirement_mix.equity_pct);
        let debt = blend(self.accumulation_debt_pct, self.retirement_mix.debt_pct);
        let theta = (Decimal::from(HUNDRED) - eq - debt).max(Decimal::ZERO);

        Ok(CashflowBucketConfig {
            equity_pct: eq,
            debt_pct: debt,
            theta_margin_pct: theta,
            ..self.retirement_mix.clone()
        })
    }
}

/// Per-bucket valuation comparison against a target [`CashflowBucketConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireBucketDriftEntry {
    pub bucket: FireBucketKind,
    pub current_value: Money,
    pub current_pct: Decimal,
    pub target_pct: Decimal,
    pub drift_pct: Decimal,
    pub rebalance_delta: Money,
}

/// Full snapshot of current portfolio vs target 3-bucket FIRE allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireBucketDrift {
    pub as_of: DateTime<Utc>,
    pub currency: Currency,
    pub total_portfolio_value: Money,
    pub buckets: Vec<FireBucketDriftEntry>,
}

/// Compares held bucket valuations against a target [`CashflowBucketConfig`].
pub fn compute_bucket_drift(
    bucket_values: &BTreeMap<FireBucketKind, Money>,
    target: &CashflowBucketConfig,
    currency: Currency,
    as_of: DateTime<Utc>,
) -> Result<FireBucketDrift, FireError> {
    target.validate()?;

    let mut total = Decimal::ZERO;
    for (&_kind, money) in bucket_values {
        if money.currency != currency {
            return Err(FireError::CurrencyMismatch {
                expected: currency,
                found: money.currency,
            });
        }
        if money.amount < Decimal::ZERO {
            return Err(FireError::InvalidInput(
                "bucket valuation cannot be negative".into(),
            ));
        }
        total += money.amount;
    }

    let hundred = Decimal::from(HUNDRED);
    let specs = [
        (FireBucketKind::GrowthEquity, target.equity_pct),
        (FireBucketKind::LiquidityRunway, target.debt_pct),
        (FireBucketKind::ThetaMargin, target.theta_margin_pct),
    ];

    let mut buckets = Vec::with_capacity(3);
    for (kind, target_pct) in specs {
        let current_amt = bucket_values
            .get(&kind)
            .map(|m| m.amount)
            .unwrap_or(Decimal::ZERO);
        let current_pct = if total.is_zero() {
            Decimal::ZERO
        } else {
            ((current_amt / total) * hundred)
                .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
        };
        let target_value = (total * target_pct / hundred)
            .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
        let delta = target_value - current_amt;
        buckets.push(FireBucketDriftEntry {
            bucket: kind,
            current_value: Money::new(current_amt, currency),
            current_pct,
            target_pct,
            drift_pct: (current_pct - target_pct)
                .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero),
            rebalance_delta: Money::new(delta, currency),
        });
    }

    Ok(FireBucketDrift {
        as_of,
        currency,
        total_portfolio_value: Money::new(
            total.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero),
            currency,
        ),
        buckets,
    })
}

/// Allocates discrete whole-lot instrument quantities for a single FIRE sleeve
/// budget (for example, sizing the `GrowthEquity` sleeve across Stage-2 leaders
/// via [`allocate_discrete`]).
pub fn allocate_fire_bucket_discrete(
    total_corpus: Money,
    bucket_pct: Decimal,
    within_bucket_weights: &TargetWeights,
    prices: &BTreeMap<InstrumentId, Price>,
    lot_sizes: &BTreeMap<InstrumentId, Quantity>,
) -> Result<BTreeMap<InstrumentId, Quantity>, AllocationError> {
    if total_corpus.amount < Decimal::ZERO {
        return Err(AllocationError::InvalidArgument(
            "total_corpus cannot be negative".into(),
        ));
    }
    if bucket_pct < Decimal::ZERO || bucket_pct > Decimal::from(HUNDRED) {
        return Err(AllocationError::InvalidArgument(
            "bucket_pct must be between 0 and 100".into(),
        ));
    }
    let sleeve_budget = Money::new(
        (total_corpus.amount * bucket_pct / Decimal::from(HUNDRED))
            .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero),
        total_corpus.currency,
    );
    allocate_discrete(within_bucket_weights, sleeve_budget, prices, lot_sizes)
}
