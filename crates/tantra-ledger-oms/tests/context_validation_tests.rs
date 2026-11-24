//! OMS context-canvas validation tests (gap 0011, plan 0005 Phase 3).
//!
//! The `OmsConfig` / `MarketData` / `PortfolioRiskState` types were declared
//! as "canvas the risk gate and broker adapters fill in" but never wired into
//! validation. These tests pin the new entry points:
//! `validate_order_against_context` and
//! `OrderStateMachine::submit_order_with_context`, plus the maintained
//! `OmsSnapshot` counters.

use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use tantra_core::{
    Exchange, InstrumentId, Money, OrderRequest, OrderSide, OrderStatus, OrderType, Price,
    Quantity, TimeInForce,
};
use tantra_ledger_oms::{
    ClientOrderId, MarketData, OmsConfig, OrderError, OrderStateMachine, PortfolioRiskState,
    validate_order_against_context,
};
use uuid::Uuid;

fn fixed_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 9, 15, 0).unwrap()
}

fn fixed_uuid(n: u128) -> Uuid {
    Uuid::from_u128(0x2026_0000_0000_0000 + n)
}

fn reliance() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "RELIANCE").unwrap()
}

fn other_scrip() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "TCS").unwrap()
}

fn sample_request(uuid: Uuid, qty: u64) -> OrderRequest {
    tantra_core::OrderRequest::with_id(
        uuid,
        reliance(),
        OrderSide::Buy,
        OrderType::Market,
        Quantity::from_u64(qty),
        "MOMENTUM_ALPHA",
    )
}

/// Market data with `last = bid = ask` by default so collar checks pass.
fn market(ask: &str, bid: &str, last: &str) -> MarketData {
    MarketData {
        instrument: reliance(),
        bid: Price::from_rupees(bid).unwrap(),
        ask: Price::from_rupees(ask).unwrap(),
        last: Price::from_rupees(last).unwrap(),
        volume: Quantity::from_u64(1_000_000),
        timestamp: fixed_time(),
    }
}

fn config(max_notional: Option<&str>, max_qty: Option<u64>, collar_bps: Option<u32>) -> OmsConfig {
    OmsConfig {
        max_notional_per_order: max_notional.map(|n| Decimal::from_str_radix(n, 10).unwrap()),
        max_quantity_per_order: max_qty.map(Quantity::from_u64),
        max_daily_loss: Some(Money::inr(Decimal::from(50_000))),
        price_collar_bps: collar_bps,
        default_time_in_force: TimeInForce::Day,
    }
}

fn risk(today_pnl: &str) -> PortfolioRiskState {
    PortfolioRiskState {
        cash: Money::inr(Decimal::from(1_000_000)),
        positions: BTreeMap::new(),
        today_pnl: Money::inr(Decimal::from_str_radix(today_pnl, 10).unwrap()),
        max_daily_loss: Money::inr(Decimal::from(50_000)),
        price_collar_bps: None,
    }
}

// ————————————————————————————————————————
// Notional ceiling
// ————————————————————————————————————————

#[test]
fn test_notional_above_configured_limit_is_rejected() {
    let req = sample_request(fixed_uuid(1), 100); // qty 100
    let cfg = config(Some("100000"), None, None); // max notional ₹1,00,000
    let md = market("2000", "1990", "1995"); // buy reference = ask 2000 → 200,000
    let err = validate_order_against_context(&req, &cfg, Some(&md), None).expect_err("rejected");
    assert!(matches!(
        err,
        OrderError::NotionalExceedsLimit { notional, max } if notional == Decimal::from(200_000) && max == Decimal::from(100_000)
    ));
}

#[test]
fn test_notional_within_configured_limit_is_approved() {
    let req = sample_request(fixed_uuid(2), 10); // 10 * 2000 = 20,000 ≤ 100,000
    let cfg = config(Some("100000"), None, None);
    let md = market("2000", "1990", "1995");
    assert!(validate_order_against_context(&req, &cfg, Some(&md), None).is_ok());
}

#[test]
fn test_notional_unchecked_when_not_configured() {
    let req = sample_request(fixed_uuid(3), 100);
    let cfg = config(None, None, None);
    let md = market("2000", "1990", "1995");
    assert!(validate_order_against_context(&req, &cfg, Some(&md), None).is_ok());
}

// ————————————————————————————————————————
// Configured quantity ceiling
// ————————————————————————————————————————

#[test]
fn test_configured_max_quantity_is_enforced() {
    let req = sample_request(fixed_uuid(4), 100);
    let cfg = config(None, Some(50), None); // config ceiling tighter than the hard 1,000,000
    let md = market("2000", "1990", "1995");
    let err = validate_order_against_context(&req, &cfg, Some(&md), None).expect_err("rejected");
    assert!(matches!(err, OrderError::QuantityExceedsLimit(_)));
}

// ————————————————————————————————————————
// Price collar
// ————————————————————————————————————————

#[test]
fn test_price_collar_violation_is_rejected() {
    // last = 1000, collar 100 bps → band ±10. Ask 1100 for a buy is outside.
    let req = sample_request(fixed_uuid(5), 10);
    let cfg = config(None, None, Some(100));
    let md = market("1100", "990", "1000");
    let err = validate_order_against_context(&req, &cfg, Some(&md), None).expect_err("rejected");
    assert!(matches!(
        err,
        OrderError::PriceCollarViolation {
            requested,
            reference,
            collar_bps,
        } if requested == Decimal::from(1100) && reference == Decimal::from(1000) && collar_bps == 100
    ));
}

#[test]
fn test_price_collar_within_band_is_approved() {
    let req = sample_request(fixed_uuid(6), 10);
    let cfg = config(None, None, Some(100));
    let md = market("1005", "995", "1000");
    assert!(validate_order_against_context(&req, &cfg, Some(&md), None).is_ok());
}

#[test]
fn test_price_collar_unchecked_when_not_configured() {
    let req = sample_request(fixed_uuid(7), 10);
    let cfg = config(None, None, None);
    let md = market("1100", "990", "1000");
    assert!(validate_order_against_context(&req, &cfg, Some(&md), None).is_ok());
}

// ————————————————————————————————————————
// Daily-loss ceiling via portfolio risk state
// ————————————————————————————————————————

#[test]
fn test_daily_loss_breach_from_risk_state_is_rejected() {
    let req = sample_request(fixed_uuid(8), 10);
    let cfg = config(None, None, None);
    let r = risk("-60000"); // worse than the -50,000 ceiling
    let err = validate_order_against_context(&req, &cfg, None, Some(&r)).expect_err("rejected");
    assert!(matches!(err, OrderError::DailyLossLimitExceeded { .. }));
}

#[test]
fn test_daily_loss_within_limit_is_approved() {
    let req = sample_request(fixed_uuid(9), 10);
    let cfg = config(None, None, None);
    let r = risk("-10000");
    assert!(validate_order_against_context(&req, &cfg, None, Some(&r)).is_ok());
}

// ————————————————————————————————————————
// Market data scrip mismatch
// ————————————————————————————————————————

#[test]
fn test_market_data_for_other_scrip_is_rejected() {
    let req = sample_request(fixed_uuid(10), 10);
    let cfg = config(None, None, None);
    let mut md = market("2000", "1990", "1995");
    md.instrument = other_scrip();
    let err = validate_order_against_context(&req, &cfg, Some(&md), None).expect_err("rejected");
    assert!(matches!(err, OrderError::InvalidParameters));
}

// ————————————————————————————————————————
// End-to-end: submit_order_with_context + snapshot
// ————————————————————————————————————————

#[test]
fn test_submit_order_with_context_registers_on_pass() {
    let mut oms = OrderStateMachine::new();
    let req = sample_request(fixed_uuid(11), 10);
    let cfg = config(Some("100000"), None, None);
    let md = market("2000", "1990", "1995");
    let ack = oms
        .submit_order_with_context(&req, fixed_time(), &cfg, Some(&md), None)
        .expect("valid context submits");
    assert_eq!(ack.status, OrderStatus::PendingNew);
    let cid = ClientOrderId::from_uuid(req.client_order_id);
    assert_eq!(oms.get_state(&cid), Some(OrderStatus::PendingNew));
}

#[test]
fn test_submit_order_with_context_rejects_notional() {
    let mut oms = OrderStateMachine::new();
    let req = sample_request(fixed_uuid(12), 100);
    let cfg = config(Some("100000"), None, None);
    let md = market("2000", "1990", "1995");
    let err = oms
        .submit_order_with_context(&req, fixed_time(), &cfg, Some(&md), None)
        .expect_err("notional exceeded");
    assert!(matches!(err, OrderError::NotionalExceedsLimit { .. }));
    // Nothing was registered.
    assert!(oms.all_order_ids().is_empty());
}

#[test]
fn test_snapshot_counters_are_maintained() {
    let mut oms = OrderStateMachine::new();
    let req = sample_request(fixed_uuid(13), 10);
    let cfg = config(None, None, None);
    oms.submit_order_with_context(&req, fixed_time(), &cfg, None, None)
        .expect("submit ok");
    let cid = ClientOrderId::from_uuid(req.client_order_id);
    oms.process_fill(&cid, Quantity::from_u64(10), fixed_time())
        .expect("fill ok");

    // A cancelled order, registered directly.
    oms.register_request_ids(
        ClientOrderId::new("clOrdX"),
        fixed_uuid(14),
        "OMS-14",
        OrderStatus::New,
        Quantity::from_u64(5),
        fixed_time(),
    )
    .expect("register ok");
    oms.cancel_order(&ClientOrderId::new("clOrdX"), fixed_time())
        .expect("cancel ok");

    let snap = oms.snapshot();
    assert_eq!(snap.total_orders, 2);
    assert_eq!(snap.total_filled, 1);
    assert_eq!(snap.total_cancelled, 1);
    assert_eq!(snap.total_commission.amount, Decimal::ZERO);
    assert_eq!(snap.total_fees.amount, Decimal::ZERO);
    assert_eq!(snap.today_pnl.amount, Decimal::ZERO);
}
