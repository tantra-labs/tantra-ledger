//! Integration tests for `tantra_ledger_performance::swr` (`T19.1` / plan 0006).

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use tantra_core::{Currency, Money};
use tantra_ledger_performance::{SwrError, SwrEvaluationInput, evaluate_swr};

fn as_of() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 18, 9, 0, 0)
        .single()
        .unwrap()
}

#[test]
fn sustainable_swr_with_passive_cashflow_buffer_preserves_capital() {
    // ₹5.4 Cr corpus, ₹18L/yr base withdrawal (3.33% SWR), 6% inflation
    // ₹19L/yr passive cashflow (~105% early coverage, growing slower/constant nominal)
    let input = SwrEvaluationInput {
        initial_corpus: Money::inr(Decimal::from(54_000_000)),
        annual_withdrawal: Money::inr(Decimal::from(1_800_000)),
        realized_period_returns: vec![
            Decimal::new(12, 2),
            Decimal::new(-8, 2),
            Decimal::new(18, 2),
            Decimal::new(10, 2),
            Decimal::new(14, 2),
        ],
        realized_income_yields: vec![
            Money::inr(Decimal::from(1_900_000)),
            Money::inr(Decimal::from(1_950_000)),
            Money::inr(Decimal::from(2_000_000)),
            Money::inr(Decimal::from(2_050_000)),
            Money::inr(Decimal::from(2_100_000)),
        ],
        inflation_pct: Decimal::from(6),
        as_of: as_of(),
    };

    let rep = evaluate_swr(&input).unwrap();
    assert!(rep.sustainable);
    assert_eq!(rep.periods_evaluated, 5);
    assert_eq!(rep.effective_withdrawal_rate_pct, Decimal::new(333, 2));
    assert!(rep.passive_income_coverage_pct > Decimal::from(95));
    assert!(rep.real_capital_preservation_pct > Decimal::from(100));
    assert!(rep.sequence_risk_drawdown_pct < Decimal::from(20));
    assert!(rep.terminal_real_corpus.amount > Decimal::from(54_000_000));
}

#[test]
fn sequence_risk_early_crash_depletes_high_withdrawal_corpus() {
    let input = SwrEvaluationInput {
        initial_corpus: Money::inr(Decimal::from(10_000_000)),
        annual_withdrawal: Money::inr(Decimal::from(2_000_000)), // 20% SWR!
        realized_period_returns: vec![
            Decimal::new(-35, 2),
            Decimal::new(-25, 2),
            Decimal::new(-15, 2),
            Decimal::new(-10, 2),
            Decimal::new(5, 2),
            Decimal::new(8, 2),
        ],
        realized_income_yields: vec![],
        inflation_pct: Decimal::from(6),
        as_of: as_of(),
    };

    let rep = evaluate_swr(&input).unwrap();
    assert!(!rep.sustainable);
    assert_eq!(rep.passive_income_coverage_pct, Decimal::ZERO);
    assert_eq!(rep.sequence_risk_drawdown_pct, Decimal::from(100));
    assert_eq!(rep.terminal_real_corpus, Money::inr(Decimal::ZERO));
    assert_eq!(rep.real_capital_preservation_pct, Decimal::ZERO);
}

#[test]
fn swr_validation_rejects_currency_mismatch_empty_returns_and_length_mismatch() {
    let base = SwrEvaluationInput {
        initial_corpus: Money::inr(Decimal::from(10_000_000)),
        annual_withdrawal: Money::new(Decimal::from(400_000), Currency::Usd),
        realized_period_returns: vec![Decimal::new(10, 2)],
        realized_income_yields: vec![],
        inflation_pct: Decimal::from(6),
        as_of: as_of(),
    };
    assert!(matches!(
        evaluate_swr(&base),
        Err(SwrError::CurrencyMismatch {
            expected: Currency::Inr,
            found: Currency::Usd
        })
    ));

    let empty = SwrEvaluationInput {
        annual_withdrawal: Money::inr(Decimal::from(400_000)),
        realized_period_returns: vec![],
        ..base.clone()
    };
    assert!(matches!(
        evaluate_swr(&empty),
        Err(SwrError::InvalidInput(_))
    ));

    let len_mismatch = SwrEvaluationInput {
        annual_withdrawal: Money::inr(Decimal::from(400_000)),
        realized_period_returns: vec![Decimal::new(10, 2), Decimal::new(12, 2)],
        realized_income_yields: vec![Money::inr(Decimal::from(100_000))],
        ..base
    };
    assert!(matches!(
        evaluate_swr(&len_mismatch),
        Err(SwrError::InvalidInput(_))
    ));
}
