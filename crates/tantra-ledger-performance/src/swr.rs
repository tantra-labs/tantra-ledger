//! Post-FIRE Safe Withdrawal Rate (SWR) & sustainability analytics (`tantra-ledger` plan 0006).
//!
//! Pure, deterministic `Decimal` evaluation over a realized or historical sequence of
//! portfolio returns and passive income yields (equity dividends, arbitrage/debt
//! interest, and systematic options theta cashflow). Zero I/O, zero clocks.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Currency, Money};

const HUNDRED: u64 = 100;

/// Errors raised when evaluating post-FIRE SWR sustainability.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SwrError {
    #[error("invalid SWR input: {0}")]
    InvalidInput(String),
    #[error("currency mismatch: expected {expected}, found {found}")]
    CurrencyMismatch { expected: Currency, found: Currency },
}

/// Inputs required to evaluate withdrawal sustainability over a realized sequence of periods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwrEvaluationInput {
    pub initial_corpus: Money,
    /// Annual inflation-baseline withdrawal in initial-year purchasing power.
    pub annual_withdrawal: Money,
    /// Realized nominal total return fraction per year (e.g. `0.12` for `+12%`).
    pub realized_period_returns: Vec<Decimal>,
    /// Optional passive income earned per year (dividends + debt interest + theta yield).
    /// When empty, zero passive income is assumed. When non-empty, length must match
    /// `realized_period_returns`.
    pub realized_income_yields: Vec<Money>,
    /// Annual CPI inflation percentage (e.g. `6` for `6%`).
    pub inflation_pct: Decimal,
    /// Explicit evaluation timestamp (`as_of`), required for temporal determinism.
    pub as_of: DateTime<Utc>,
}

/// Quantitative SWR sustainability scorecard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwrReport {
    pub as_of: DateTime<Utc>,
    pub currency: Currency,
    pub periods_evaluated: usize,
    pub effective_withdrawal_rate_pct: Decimal,
    pub passive_income_coverage_pct: Decimal,
    pub real_capital_preservation_pct: Decimal,
    pub sequence_risk_drawdown_pct: Decimal,
    pub terminal_real_corpus: Money,
    pub sustainable: bool,
}

fn round_2dp(v: Decimal) -> Decimal {
    v.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// Evaluates whether a withdrawal schedule survives a sequence of returns and
/// measures how much of the living expense was buffered by passive cashflow.
pub fn evaluate_swr(input: &SwrEvaluationInput) -> Result<SwrReport, SwrError> {
    if input.initial_corpus.amount <= Decimal::ZERO {
        return Err(SwrError::InvalidInput(
            "initial_corpus must be strictly positive".into(),
        ));
    }
    if input.annual_withdrawal.currency != input.initial_corpus.currency {
        return Err(SwrError::CurrencyMismatch {
            expected: input.initial_corpus.currency,
            found: input.annual_withdrawal.currency,
        });
    }
    if input.annual_withdrawal.amount < Decimal::ZERO {
        return Err(SwrError::InvalidInput(
            "annual_withdrawal cannot be negative".into(),
        ));
    }
    if input.realized_period_returns.is_empty() {
        return Err(SwrError::InvalidInput(
            "realized_period_returns cannot be empty".into(),
        ));
    }
    if input.inflation_pct <= -Decimal::from(HUNDRED) {
        return Err(SwrError::InvalidInput(
            "inflation_pct must be > -100".into(),
        ));
    }
    if !input.realized_income_yields.is_empty()
        && input.realized_income_yields.len() != input.realized_period_returns.len()
    {
        return Err(SwrError::InvalidInput(
            "realized_income_yields must be empty or match realized_period_returns length".into(),
        ));
    }
    for m in &input.realized_income_yields {
        if m.currency != input.initial_corpus.currency {
            return Err(SwrError::CurrencyMismatch {
                expected: input.initial_corpus.currency,
                found: m.currency,
            });
        }
        if m.amount < Decimal::ZERO {
            return Err(SwrError::InvalidInput(
                "realized_income_yields cannot contain negative entries".into(),
            ));
        }
    }

    let hundred = Decimal::from(HUNDRED);
    let cur = input.initial_corpus.currency;
    let inf_mult = Decimal::ONE + (input.inflation_pct / hundred);

    let mut nominal_corpus = input.initial_corpus.amount;
    let mut cum_inf = Decimal::ONE;
    let mut peak_real = input.initial_corpus.amount;
    let mut worst_dd = Decimal::ZERO;
    let mut total_passive_real = Decimal::ZERO;
    let mut total_withdrawal_real = Decimal::ZERO;
    let mut depleted = false;

    for (idx, &ret_nom) in input.realized_period_returns.iter().enumerate() {
        let withdrawal_nom = input.annual_withdrawal.amount * cum_inf;
        let passive_nom = input
            .realized_income_yields
            .get(idx)
            .map(|m| m.amount)
            .unwrap_or(Decimal::ZERO);

        total_passive_real += passive_nom / cum_inf;
        total_withdrawal_real += input.annual_withdrawal.amount;

        // Only withdraw net principal gap above passive income; excess passive stays invested.
        let net_draw_nom = withdrawal_nom - passive_nom;
        nominal_corpus = (nominal_corpus - net_draw_nom) * (Decimal::ONE + ret_nom);
        cum_inf *= inf_mult;

        if nominal_corpus <= Decimal::ZERO {
            nominal_corpus = Decimal::ZERO;
            depleted = true;
            worst_dd = Decimal::ONE;
            break;
        }

        let real_val = nominal_corpus / cum_inf;
        if real_val > peak_real {
            peak_real = real_val;
        }
        let dd = if peak_real > Decimal::ZERO {
            (peak_real - real_val) / peak_real
        } else {
            Decimal::ZERO
        };
        if dd > worst_dd {
            worst_dd = dd;
        }
    }

    let terminal_real = if depleted {
        Decimal::ZERO
    } else {
        round_2dp(nominal_corpus / cum_inf)
    };
    let effective_swr_pct =
        round_2dp((input.annual_withdrawal.amount / input.initial_corpus.amount) * hundred);
    let coverage_pct = if total_withdrawal_real.is_zero() {
        hundred
    } else {
        round_2dp((total_passive_real / total_withdrawal_real) * hundred)
    };
    let preservation_pct = round_2dp((terminal_real / input.initial_corpus.amount) * hundred);
    let dd_pct = round_2dp(worst_dd * hundred);

    Ok(SwrReport {
        as_of: input.as_of,
        currency: cur,
        periods_evaluated: input.realized_period_returns.len(),
        effective_withdrawal_rate_pct: effective_swr_pct,
        passive_income_coverage_pct: coverage_pct,
        real_capital_preservation_pct: preservation_pct,
        sequence_risk_drawdown_pct: dd_pct,
        terminal_real_corpus: Money::new(terminal_real, cur),
        sustainable: !depleted && terminal_real > Decimal::ZERO,
    })
}
