//! Bracket FIFO booking adapter tests (gap 0012 follow-on, plan 0005 T12.7).
//!
//! `BracketOrder::fifo_exits` projects the journaled `PartialExit` +
//! `StopTriggered` events into ordered `(price, qty)` exit tuples, and feeding
//! those tuples to `tantra_ledger_portfolio::apply_bracket_exits` on a fresh
//! account reproduces the bracket's own audit realized P&L exactly (FIFO ==
//! anchor when the bracket lot is the only lot in the account).

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use tantra_core::{
    Exchange, FillReport, InstrumentId, OrderRequest, OrderSide, OrderType, PlannedTarget, Price,
    Quantity, TrailRule,
};
use tantra_ledger_oms::{BracketEvent, BracketOrder};
use tantra_ledger_portfolio::{
    AccountState, Currency, HeldPosition, Money, PositionSide, apply_bracket_exits,
};
use uuid::Uuid;

fn t1() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 9, 30, 0).unwrap()
}

fn t2() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 10, 0, 0).unwrap()
}

fn t3() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 11, 0, 0).unwrap()
}

fn t4() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 12, 0, 0).unwrap()
}

fn fixed_uuid(n: u128) -> Uuid {
    Uuid::from_u128(0x2026_0000_0000_0000 + n)
}

fn reliance() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "RELIANCE").unwrap()
}

fn entry_request(uuid: Uuid, side: OrderSide, limit: &str, qty: u64) -> OrderRequest {
    OrderRequest::with_id(
        uuid,
        reliance(),
        side,
        OrderType::Limit(Price::from_rupees(limit).unwrap()),
        Quantity::from_u64(qty),
        "EE_BRACKET",
    )
}

fn target(price: &str, qty: u64, reason: &str) -> PlannedTarget {
    PlannedTarget {
        price: Price::from_rupees(price).unwrap(),
        qty: Quantity::from_u64(qty),
        reason: reason.to_string(),
    }
}

fn fill(uuid: Uuid, price: &str, qty: u64, side: OrderSide, at: DateTime<Utc>) -> FillReport {
    FillReport::new(
        format!("fill-{uuid}"),
        format!("order-{uuid}"),
        uuid,
        reliance(),
        side,
        Price::from_rupees(price).unwrap(),
        Quantity::from_u64(qty),
        "EE_BRACKET",
        at,
        "acct-1",
    )
}

fn account_pos(side: PositionSide, qty: u64, price: &str) -> AccountState {
    AccountState::new(
        Currency::Inr,
        vec![HeldPosition::new(
            reliance(),
            side,
            Quantity::from_u64(qty),
            Price::from_rupees(price).unwrap(),
        )],
    )
}

/// Full long lifecycle (entry → partial ⅓ → trail → stop on runner): the
/// FIFO exit tuples must match the journaled exit events in fill order, and
/// booking them on a fresh account must reproduce the bracket's audit P&L.
#[test]
fn test_fifo_exits_match_journal_and_book_bracket_pnl_long() {
    let mut bracket = BracketOrder::new(
        entry_request(fixed_uuid(1), OrderSide::Buy, "1000", 300),
        Price::from_rupees("950").unwrap(),
        vec![
            target("1100", 100, "target 1 at 2R"),
            target("1150", 100, "target 2 at 3R"),
            target("1200", 100, "target 3 at 4R"),
        ],
        Some(TrailRule::Percent(Decimal::from(2))),
    )
    .expect("valid long bracket");

    bracket
        .on_entry_fill(
            &fill(fixed_uuid(1), "1000", 300, OrderSide::Buy, t1()),
            t1(),
        )
        .expect("entry fill ok");
    bracket
        .on_partial_exit(
            Price::from_rupees("1100").unwrap(),
            Quantity::from_u64(100),
            "target 1 at 2R",
            t2(),
        )
        .expect("partial exit ok");
    bracket
        .apply_stop_update(
            Price::from_rupees("1000").unwrap(),
            Some(Quantity::from_u64(200)),
            t2(),
        )
        .expect("stop resize ok");
    bracket
        .apply_stop_update(Price::from_rupees("1050").unwrap(), None, t3())
        .expect("trail ok");
    bracket
        .on_stop_trigger(Price::from_rupees("1050").unwrap(), t4())
        .expect("stop trigger ok");
    assert!(bracket.is_closed());

    // The exit tuple stream exactly matches the journaled exit events in order.
    let exits = bracket.fifo_exits();
    let journaled: Vec<(Price, Quantity)> = bracket
        .events()
        .iter()
        .filter_map(|ev| match ev {
            BracketEvent::PartialExit { price, qty, .. }
            | BracketEvent::StopTriggered { price, qty, .. } => Some((*price, *qty)),
            _ => None,
        })
        .collect();
    assert_eq!(exits, journaled);
    assert_eq!(exits.len(), 2);
    assert_eq!(
        exits[0],
        (Price::from_rupees("1100").unwrap(), Quantity::from_u64(100))
    );
    assert_eq!(
        exits[1],
        (Price::from_rupees("1050").unwrap(), Quantity::from_u64(200))
    );

    // Booking the tuples FIFO on a fresh account (only the bracket's lot)
    // reproduces the bracket's audit realized P&L — the sum of every journaled
    // exit leg, single-anchored at the bracket's entry fill.
    let mut account = account_pos(PositionSide::Long, 300, "1000");
    let pnl = apply_bracket_exits(&mut account, &reliance(), PositionSide::Long, &exits);

    // Consistency check: the booked P&L equals the bracket's own audit sum over
    // the same journaled exit events (anchored at the entry fill price 1000):
    //   (1100−1000)·100 + (1050−1000)·200 = 20,000.
    let audit_sum: Decimal = bracket
        .events()
        .iter()
        .filter_map(|ev| match ev {
            BracketEvent::PartialExit { price, qty, .. }
            | BracketEvent::StopTriggered { price, qty, .. } => {
                Some((price.value() - Decimal::from(1000)) * qty.value())
            }
            _ => None,
        })
        .sum();
    assert_eq!(audit_sum, Decimal::from(20_000));
    assert_eq!(pnl, Money::inr(audit_sum));
    assert_eq!(account.positions[0].quantity, Quantity::ZERO);
}

/// Short bracket (entry → partial exit ½ → trail down → stop on runner): the
/// FIFO booking reproduces the bracket's audit P&L with a negative stop leg.
#[test]
fn test_fifo_exits_book_bracket_pnl_short() {
    let mut bracket = BracketOrder::new(
        entry_request(fixed_uuid(2), OrderSide::Sell, "1000", 200),
        Price::from_rupees("1050").unwrap(), // short stop above the entry
        vec![target("950", 100, "target 1 at 1R")],
        None,
    )
    .expect("valid short bracket");

    bracket
        .on_entry_fill(
            &fill(fixed_uuid(2), "1000", 200, OrderSide::Sell, t1()),
            t1(),
        )
        .expect("entry fill ok");
    bracket
        .on_partial_exit(
            Price::from_rupees("950").unwrap(),
            Quantity::from_u64(100),
            "target 1 at 1R",
            t2(),
        )
        .expect("partial exit ok");
    bracket
        .apply_stop_update(
            Price::from_rupees("1040").unwrap(),
            Some(Quantity::from_u64(100)),
            t3(),
        )
        .expect("trail down ok");
    bracket
        .on_stop_trigger(Price::from_rupees("1040").unwrap(), t4())
        .expect("stop trigger ok");
    assert!(bracket.is_closed());

    let exits = bracket.fifo_exits();
    assert_eq!(
        exits,
        vec![
            (Price::from_rupees("950").unwrap(), Quantity::from_u64(100)),
            (Price::from_rupees("1040").unwrap(), Quantity::from_u64(100)),
        ]
    );

    // Booking on a fresh short account (only the bracket's lot) reproduces the
    // bracket's audit sum over the journaled exit events (anchored at the
    // entry fill price 1000): (1000 − 950) × 100 + (1000 − 1040) × 100 = 1,000.
    let mut account = account_pos(PositionSide::Short, 200, "1000");
    let pnl = apply_bracket_exits(&mut account, &reliance(), PositionSide::Short, &exits);

    let audit_sum: Decimal = bracket
        .events()
        .iter()
        .filter_map(|ev| match ev {
            BracketEvent::PartialExit { price, qty, .. }
            | BracketEvent::StopTriggered { price, qty, .. } => {
                Some((Decimal::from(1000) - price.value()) * qty.value())
            }
            _ => None,
        })
        .sum();
    assert_eq!(audit_sum, Decimal::from(1_000));
    assert_eq!(pnl, Money::inr(audit_sum));
    assert_eq!(account.positions[0].quantity, Quantity::ZERO);
    assert_eq!(account.positions[0].side, PositionSide::Short);
}

/// An unfilled / open bracket (no entry fill) has no exit legs yet.
#[test]
fn test_fifo_exits_empty_before_any_exit() {
    let bracket = BracketOrder::new(
        entry_request(fixed_uuid(3), OrderSide::Buy, "1000", 100),
        Price::from_rupees("900").unwrap(),
        vec![],
        None,
    )
    .expect("valid bracket");
    assert!(bracket.fifo_exits().is_empty());

    // An open bracket after the entry fill but before any exit also has none.
    let mut bracket = BracketOrder::new(
        entry_request(fixed_uuid(4), OrderSide::Buy, "1000", 100),
        Price::from_rupees("900").unwrap(),
        vec![target("1100", 100, "target 1")],
        None,
    )
    .expect("valid bracket");
    bracket
        .on_entry_fill(
            &fill(fixed_uuid(4), "1000", 100, OrderSide::Buy, t1()),
            t1(),
        )
        .expect("entry fill ok");
    assert!(bracket.fifo_exits().is_empty());
    assert!(!bracket.is_closed());
}
