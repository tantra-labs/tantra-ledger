//! Currency-checked aggregation across closed trades.

mod common;

use chrono::NaiveDate;
use common::*;
use rust_decimal::Decimal;
use tantra_core::{CoreError, Currency, Money};
use tantra_ledger_performance::calendar::{build_calendar, daily_pnl_buckets, total_fees};
use tantra_ledger_performance::metrics::{MetricsError, MetricsInput, compute_metrics};
use tantra_ledger_performance::model::ClosedTrade;

fn date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, 10).unwrap()
}

fn trade(currency: Currency) -> ClosedTrade {
    let mut lot = matched_lot("L", "100", "110", 10, 1, false);
    lot.realized_pnl = Money::new(lot.realized_pnl.amount(), currency);
    closed_trade_from_lot(&lot, date())
}

fn input(trades: Vec<ClosedTrade>) -> MetricsInput {
    MetricsInput::new(trades, now())
}

#[test]
fn mixed_currencies_are_rejected_everywhere() {
    let trades = vec![trade(Currency::Inr), trade(Currency::Usd)];
    assert!(matches!(
        compute_metrics(&input(trades.clone())),
        Err(MetricsError::CurrencyMismatch { .. })
    ));
    assert!(build_calendar(&trades, 2026, 3).is_err());
    assert!(daily_pnl_buckets(&trades).is_err());
    assert!(total_fees(&trades).is_err());
}

#[test]
fn all_usd_trades_produce_usd_outputs() {
    let trades = vec![trade(Currency::Usd), trade(Currency::Usd)];
    assert_eq!(total_fees(&trades).unwrap().currency(), Currency::Usd);
    let m = compute_metrics(&input(trades.clone())).unwrap();
    assert_eq!(m.net_pnl.currency(), Currency::Usd);
    assert_eq!(m.fees.currency(), Currency::Usd);
    let buckets = daily_pnl_buckets(&trades).unwrap();
    assert_eq!(buckets[&date()].net_pnl.currency(), Currency::Usd);
}

#[test]
fn zero_foreign_fee_accepted_nonzero_rejected() {
    let mut t = trade(Currency::Usd);
    t.fees = Money::zero(Currency::Inr);
    assert_eq!(t.currency().unwrap(), Currency::Usd);
    t.fees = Money::new(Decimal::ONE, Currency::Inr);
    assert!(matches!(
        t.currency(),
        Err(CoreError::CurrencyMismatch { .. })
    ));
}
