//! Golden contract-note report tests.
//!
//! Pins the whole [`GoldenTestRunner`] report over the five built-in samples
//! (fixed `now`, fixed tolerance): the calculated-vs-charged fields, the
//! maximum per-trade deviation in paise, and the report's JSON round-trip.
//!
//! The runner validates the **canonical** track (`TaxCalculator::compute_charges`,
//! plan 0005 gap 0010) — not the retired legacy floor-rounded track. The
//! sample fixtures were re-based 2026-11-13 onto the canonical track, so the
//! whole report passes at the 1-paise tolerance: a change in any statutory
//! formula shows up as a different calculated value.

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use tantra_ledger_journal::{
    GoldenTestRunner, sample_total_charges_trade, sample_upstox_delivery_buy_trade,
    sample_upstox_option_buy_trade, sample_zerodha_delivery_buy_trade,
    sample_zerodha_intraday_sell_trade,
};

/// Fixed stamp injected by the tests, standing in for the deleted
/// `chrono::Utc::now()` (temporal determinism).
fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 9, 15, 0).unwrap()
}

/// All five built-in samples at the given tolerance.
fn full_sample_report(tolerance_paise: i64) -> tantra_ledger_journal::GoldenTestReport {
    GoldenTestRunner::new()
        .with_tolerance(Decimal::from(tolerance_paise))
        .add_trade(sample_zerodha_delivery_buy_trade(fixed_now()))
        .add_trade(sample_zerodha_intraday_sell_trade(fixed_now()))
        .add_trade(sample_upstox_delivery_buy_trade(fixed_now()))
        .add_trade(sample_upstox_option_buy_trade(fixed_now()))
        .add_trade(sample_total_charges_trade(fixed_now()))
        .run()
}

#[test]
fn test_golden_all_samples_pass_at_sampling_tolerance() {
    let report = full_sample_report(5000);
    assert_eq!(report.total, 5);
    assert_eq!(report.passed, 5);
    assert_eq!(report.failed, 0);
    assert_eq!(report.pass_rate(), 100.0);
    assert!(report.failed_trades().is_empty());
}

#[test]
fn test_golden_all_samples_pass_at_one_paise() {
    // Re-based onto the canonical track (gap 0010): the statutory fields and
    // the net amount all reproduce the fixtures within 1 paise.
    let report = full_sample_report(1);
    assert_eq!(report.passed, 5);
    assert_eq!(report.failed, 0);
    assert!(report.stt_mismatches().is_empty());
    assert!(report.net_amount_mismatches().is_empty());
}

#[test]
fn test_golden_statutory_fields_match_exactly() {
    let report = full_sample_report(1);
    let mut by_id = report.results.iter().map(|r| {
        (
            r.trade_id.clone(),
            (
                r.stt_calculated,
                r.exchange_calculated,
                r.sebi_calculated,
                r.stamp_duty_calculated,
                r.gst_calculated,
                r.dp_charges_calculated,
            ),
        )
    });
    let mut field =
        |id: &str, stt: i64, exchange: i64, sebi: i64, stamp: i64, gst: i64, dp: i64| {
            let (s, e, sb, sd, g, d) = by_id.next().unwrap().1;
            assert_eq!(s, Decimal::from(stt), "{id} STT");
            assert_eq!(e, Decimal::from(exchange), "{id} exchange");
            assert_eq!(sb, Decimal::from(sebi), "{id} SEBI");
            assert_eq!(sd, Decimal::from(stamp), "{id} stamp duty");
            assert_eq!(g, Decimal::from(gst), "{id} GST");
            assert_eq!(d, Decimal::from(dp), "{id} DP charges");
        };

    // Zerodha delivery buy: 100 @ ₹2,500 (NSE, canonical NSE 0.00297% rate).
    field("ZERODHA_DELIVERY_BUY_001", 25_000, 742, 25, 3_750, 498, 0);
    // Zerodha intraday sell: 50 @ ₹1,500 (NSE).
    field("ZERODHA_INTRADAY_SELL_001", 1_875, 223, 8, 0, 402, 0);
    // Upstox delivery buy: 200 @ ₹3,200 (NSE).
    field("UPSTOX_DELIVERY_BUY_001", 64_000, 1_901, 64, 9_600, 714, 0);
    // Upstox option buy: 1 lot = 50 @ ₹150 premium (NFO) — options STT is
    // sell-side only so a buy pays 0; the exchange charge uses the per-segment
    // NFO 0.03553% premium rate (gap 0009).
    field("UPSTOX_OPTION_BUY_001", 0, 266, 1, 22, 89, 0);
    // Total-charges sample: 100 @ ₹2,000 (NSE); DP is zero on a buy.
    field("TOTAL_CHARGES_TEST_001", 20_000, 594, 20, 3_000, 471, 0);
}

#[test]
fn test_golden_max_deviation_is_zero_at_one_paise() {
    // Fully re-based: no field deviates from its fixture at the 1-paise
    // tolerance (the canonical track IS the recorded contract note).
    let report = full_sample_report(1);
    for result in &report.results {
        assert_eq!(
            result.max_deviation_paise,
            Decimal::ZERO,
            "{}",
            result.trade_id
        );
        assert_eq!(
            result.net_amount_calculated, result.net_amount_broker,
            "{}",
            result.trade_id
        );
    }
}

#[test]
fn test_golden_report_json_round_trip() {
    let report = full_sample_report(5000);
    let json = report.to_json().expect("report serializes");
    let parsed: tantra_ledger_journal::GoldenTestReport =
        serde_json::from_str(&json).expect("report JSON round-trips");
    assert_eq!(parsed.total, report.total);
    assert_eq!(parsed.passed, report.passed);
    assert_eq!(parsed.failed, report.failed);
    assert_eq!(parsed.tolerance_paise, report.tolerance_paise);
    assert_eq!(parsed.results.len(), report.results.len());
    for (a, b) in parsed.results.iter().zip(&report.results) {
        assert_eq!(a.trade_id, b.trade_id);
        assert_eq!(a.all_match, b.all_match);
        assert_eq!(a.max_deviation_paise, b.max_deviation_paise);
    }
}
