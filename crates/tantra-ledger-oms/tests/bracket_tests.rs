//! Bracket-order lifecycle tests (gap 0012).
//!
//! Full multi-leg bracket lifecycle (entry → partial exit ⅓ + SL qty resize →
//! trail → stop trigger on the runner → close), golden PnL vectors, no
//! double-count / no short-flip guards, stop-widening refusal on both sides,
//! FIFO target-leg consumption, and `apply_fill_to_bracket` leg routing.

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use tantra_core::{
    Exchange, FillReport, InstrumentId, OrderRequest, OrderSide, OrderType, PlannedTarget, Price,
    Quantity, TrailRule,
};
use tantra_ledger_oms::{BracketEvent, BracketOrder, OrderError};
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
    InstrumentId::new(Exchange::Nse, "RELIANCE")
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

/// Standard long bracket: entry 1000 × 300, initial stop 950, targets at
/// 1100/1150/1200 (100 shares each), 2% trailing rule.
fn long_bracket() -> BracketOrder {
    BracketOrder::new(
        entry_request(fixed_uuid(1), OrderSide::Buy, "1000", 300),
        Price::from_rupees("950").unwrap(),
        vec![
            target("1100", 100, "target 1 at 2R"),
            target("1150", 100, "target 2 at 3R"),
            target("1200", 100, "target 3 at 4R"),
        ],
        Some(TrailRule::Percent(Decimal::from(2))),
    )
    .expect("valid long bracket")
}

// ————————————————————————————————————————————————————————————
// Full multi-leg lifecycle + golden PnL
// ————————————————————————————————————————————————————————————

#[test]
fn test_full_lifecycle_entry_partial_exit_trail_stop_trigger_close() {
    let mut bracket = long_bracket();

    // 1. Entry fill: position opens, stop qty matches the open qty.
    bracket
        .on_entry_fill(
            &fill(fixed_uuid(1), "1000", 300, OrderSide::Buy, t1()),
            t1(),
        )
        .expect("entry fill ok");
    assert_eq!(bracket.open_qty(), Quantity::from_u64(300));
    assert_eq!(bracket.resting_stop_qty(), Quantity::from_u64(300));
    assert_eq!(bracket.resting_stop(), Price::from_rupees("950").unwrap());
    assert!(!bracket.is_closed());
    assert_eq!(bracket.remaining_targets.len(), 3);

    // 2. Partial exit ⅓ at target 1 (2R) + stop-quantity resize to the runner.
    bracket
        .on_partial_exit(
            Price::from_rupees("1100").unwrap(),
            Quantity::from_u64(100),
            "target 1 at 2R",
            t2(),
        )
        .expect("partial exit ok");
    assert_eq!(bracket.open_qty(), Quantity::from_u64(200));
    // No double-count and no short-flip: open + sum(partial exits) == entry qty.
    let partial_exits: Decimal = bracket
        .events()
        .iter()
        .filter_map(|ev| match ev {
            BracketEvent::PartialExit { qty, .. } => Some(qty.value()),
            _ => None,
        })
        .sum();
    assert_eq!(
        bracket.open_qty().value() + partial_exits,
        Decimal::from(300)
    );

    bracket
        .apply_stop_update(
            Price::from_rupees("1000").unwrap(),
            Some(Quantity::from_u64(200)),
            t2(),
        )
        .expect("stop resize ok");
    assert_eq!(bracket.resting_stop(), Price::from_rupees("1000").unwrap());
    assert_eq!(bracket.resting_stop_qty(), Quantity::from_u64(200));
    // The bracket stays open for the runner (not a premature close on the ⅓).
    assert!(!bracket.is_closed());
    assert_eq!(bracket.open_qty(), Quantity::from_u64(200));

    // 3. Trail: the stop moves up for a long (allowed — never widening).
    bracket
        .apply_stop_update(Price::from_rupees("1050").unwrap(), None, t3())
        .expect("trail ok");
    assert_eq!(bracket.resting_stop(), Price::from_rupees("1050").unwrap());

    // 4. Stop trigger on the runner at the trailing stop level.
    bracket
        .on_stop_trigger(Price::from_rupees("1050").unwrap(), t4())
        .expect("stop trigger ok");
    assert!(bracket.is_closed());
    assert_eq!(bracket.open_qty(), Quantity::ZERO);
    assert_eq!(bracket.resting_stop_qty(), Quantity::ZERO);

    // Golden vector: realized PnL = (1050 − 1000) × 200 = 10,000, exact Decimal.
    let golden_pnl = (Decimal::from(1050) - Decimal::from(1000)) * Decimal::from(200);
    assert_eq!(golden_pnl, Decimal::from(10_000));

    let closed = bracket
        .events()
        .iter()
        .find_map(|ev| match ev {
            BracketEvent::Closed { realized_pnl, .. } => Some(*realized_pnl),
            _ => None,
        })
        .expect("Closed event journaled");
    assert_eq!(closed, golden_pnl);
    assert_eq!(closed, Decimal::from(10_000));

    // Event journal order: EntryFilled → PartialExit → StopMoved → StopMoved → StopTriggered → Closed.
    let kinds: Vec<&str> = bracket
        .events()
        .iter()
        .map(|ev| match ev {
            BracketEvent::EntryFilled { .. } => "entry_filled",
            BracketEvent::StopMoved { .. } => "stop_moved",
            BracketEvent::PartialExit { .. } => "partial_exit",
            BracketEvent::StopTriggered { .. } => "stop_triggered",
            BracketEvent::Closed { .. } => "closed",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "entry_filled",
            "partial_exit",
            "stop_moved",
            "stop_moved",
            "stop_triggered",
            "closed"
        ]
    );
}

// ————————————————————————————————————————————————————————————
// Stop-widening refusal on both sides
// ————————————————————————————————————————————————————————————

#[test]
fn test_stop_widening_refused_for_long() {
    let mut bracket = long_bracket();
    bracket
        .on_entry_fill(
            &fill(fixed_uuid(1), "1000", 300, OrderSide::Buy, t1()),
            t1(),
        )
        .expect("entry fill ok");

    let err = bracket
        .apply_stop_update(Price::from_rupees("940").unwrap(), None, t2())
        .expect_err("widening must be refused");
    assert!(matches!(
        err,
        OrderError::StopWidening { current, new }
            if current == Price::from_rupees("950").unwrap()
                && new == Price::from_rupees("940").unwrap()
    ));
    // Nothing mutated.
    assert_eq!(bracket.resting_stop(), Price::from_rupees("950").unwrap());

    // Tightening upward (trailing) is allowed.
    bracket
        .apply_stop_update(Price::from_rupees("970").unwrap(), None, t2())
        .expect("tightening allowed");
    assert_eq!(bracket.resting_stop(), Price::from_rupees("970").unwrap());
}

#[test]
fn test_stop_widening_refused_for_short() {
    let mut bracket = BracketOrder::new(
        entry_request(fixed_uuid(2), OrderSide::Sell, "1000", 200),
        Price::from_rupees("1050").unwrap(), // short stop above the entry
        vec![target("950", 200, "target 1 at 1R")],
        None,
    )
    .expect("valid short bracket");
    bracket
        .on_entry_fill(
            &fill(fixed_uuid(2), "1000", 200, OrderSide::Sell, t1()),
            t1(),
        )
        .expect("entry fill ok");

    // Widening for a short = moving the stop up, away from the entry.
    let err = bracket
        .apply_stop_update(Price::from_rupees("1060").unwrap(), None, t2())
        .expect_err("widening must be refused");
    assert!(matches!(
        err,
        OrderError::StopWidening { current, new }
            if current == Price::from_rupees("1050").unwrap()
                && new == Price::from_rupees("1060").unwrap()
    ));
    assert_eq!(bracket.resting_stop(), Price::from_rupees("1050").unwrap());

    // Tightening downward (trailing for a short) is allowed.
    bracket
        .apply_stop_update(Price::from_rupees("1040").unwrap(), None, t2())
        .expect("tightening allowed");
    assert_eq!(bracket.resting_stop(), Price::from_rupees("1040").unwrap());
}

// ————————————————————————————————————————————————————————————
// Construction validation
// ————————————————————————————————————————————————————————————

#[test]
fn test_new_rejects_stop_on_wrong_side() {
    // Buy: stop must be strictly below the entry limit.
    let err = BracketOrder::new(
        entry_request(fixed_uuid(3), OrderSide::Buy, "1000", 100),
        Price::from_rupees("1000").unwrap(),
        vec![],
        None,
    )
    .expect_err("buy stop at/above entry is invalid");
    assert!(matches!(err, OrderError::InvalidBracket(_)));

    // Sell: stop must be strictly above the entry limit.
    let err = BracketOrder::new(
        entry_request(fixed_uuid(4), OrderSide::Sell, "1000", 100),
        Price::from_rupees("1000").unwrap(),
        vec![],
        None,
    )
    .expect_err("sell stop at/below entry is invalid");
    assert!(matches!(err, OrderError::InvalidBracket(_)));

    // Valid constructions still pass.
    assert!(
        BracketOrder::new(
            entry_request(fixed_uuid(5), OrderSide::Buy, "1000", 100),
            Price::from_rupees("900").unwrap(),
            vec![],
            None,
        )
        .is_ok()
    );
    assert!(
        BracketOrder::new(
            entry_request(fixed_uuid(6), OrderSide::Sell, "1000", 100),
            Price::from_rupees("1100").unwrap(),
            vec![],
            None,
        )
        .is_ok()
    );
}

#[test]
fn test_new_rejects_non_positive_target_quantity() {
    let err = BracketOrder::new(
        entry_request(fixed_uuid(7), OrderSide::Buy, "1000", 100),
        Price::from_rupees("900").unwrap(),
        vec![target("1100", 0, "zero-qty target")],
        None,
    )
    .expect_err("zero-qty target is invalid");
    assert!(matches!(err, OrderError::InvalidBracket(_)));
}

// ————————————————————————————————————————————————————————————
// Qty guards: stop-qty ceiling and partial-exit short-flip
// ————————————————————————————————————————————————————————————

#[test]
fn test_stop_quantity_cannot_exceed_open() {
    let mut bracket = long_bracket();
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
            "target 1",
            t2(),
        )
        .expect("partial exit ok");

    // Resting stop qty must never exceed the remaining 200.
    let err = bracket
        .apply_stop_update(
            Price::from_rupees("1000").unwrap(),
            Some(Quantity::from_u64(201)),
            t2(),
        )
        .expect_err("stop qty exceeds open");
    assert!(matches!(err, OrderError::StopQtyExceedsOpen));
    assert_eq!(bracket.resting_stop_qty(), Quantity::from_u64(300));
}

#[test]
fn test_partial_exit_cannot_short_flip() {
    let mut bracket = long_bracket();
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
            "target 1",
            t2(),
        )
        .expect("partial exit ok");

    // Exiting more than the remaining 200 would flip the position short.
    let err = bracket
        .on_partial_exit(
            Price::from_rupees("1150").unwrap(),
            Quantity::from_u64(201),
            "target 2",
            t3(),
        )
        .expect_err("short flip refused");
    assert!(matches!(err, OrderError::QuantityMismatch));
    assert_eq!(bracket.open_qty(), Quantity::from_u64(200));
}

// ————————————————————————————————————————————————————————————
// FIFO target-leg consumption
// ————————————————————————————————————————————————————————————

#[test]
fn test_remaining_targets_consumed_fifo_by_position() {
    let mut bracket = long_bracket();
    bracket
        .on_entry_fill(
            &fill(fixed_uuid(1), "1000", 300, OrderSide::Buy, t1()),
            t1(),
        )
        .expect("entry fill ok");

    // First ⅓ exit removes the first remaining target (FIFO).
    bracket
        .on_partial_exit(
            Price::from_rupees("1100").unwrap(),
            Quantity::from_u64(100),
            "target 1",
            t2(),
        )
        .expect("exit 1 ok");
    let remaining: Vec<&str> = bracket
        .remaining_targets
        .iter()
        .map(|t| t.reason.as_str())
        .collect();
    assert_eq!(remaining, ["target 2 at 3R", "target 3 at 4R"]);

    // Second ⅓ exit removes the next target in order.
    bracket
        .on_partial_exit(
            Price::from_rupees("1150").unwrap(),
            Quantity::from_u64(100),
            "target 2",
            t3(),
        )
        .expect("exit 2 ok");
    let remaining: Vec<&str> = bracket
        .remaining_targets
        .iter()
        .map(|t| t.reason.as_str())
        .collect();
    assert_eq!(remaining, ["target 3 at 4R"]);
}

// ————————————————————————————————————————————————————————————
// apply_fill_to_bracket leg routing
// ————————————————————————————————————————————————————————————

#[test]
fn test_apply_fill_to_bracket_routes_entry_and_target_legs() {
    let mut bracket = long_bracket();

    // Entry leg: same client order id + entry quantity → on_entry_fill.
    bracket
        .apply_fill_to_bracket(
            &fill(fixed_uuid(1), "1000", 300, OrderSide::Buy, t1()),
            t1(),
        )
        .expect("entry routed");
    assert_eq!(bracket.open_qty(), Quantity::from_u64(300));
    assert!(bracket.entry_fill.is_some());

    // Target leg: quantity matches a remaining target → on_partial_exit.
    bracket
        .apply_fill_to_bracket(
            &fill(fixed_uuid(11), "1100", 100, OrderSide::Sell, t2()),
            t2(),
        )
        .expect("target leg routed");
    assert_eq!(bracket.open_qty(), Quantity::from_u64(200));
    assert_eq!(bracket.remaining_targets.len(), 2);

    // Unmatched fill is a no-op.
    bracket
        .apply_fill_to_bracket(
            &fill(fixed_uuid(12), "1090", 77, OrderSide::Sell, t2()),
            t2(),
        )
        .expect("unmatched no-op");
    assert_eq!(bracket.open_qty(), Quantity::from_u64(200));
    assert_eq!(bracket.remaining_targets.len(), 2);

    // Fills after close are idempotent no-ops.
    bracket
        .on_stop_trigger(Price::from_rupees("1050").unwrap(), t3())
        .expect("close ok");
    assert!(bracket.is_closed());
    bracket
        .apply_fill_to_bracket(
            &fill(fixed_uuid(13), "1100", 100, OrderSide::Sell, t3()),
            t3(),
        )
        .expect("post-close no-op");
    assert!(bracket.is_closed());
    assert_eq!(bracket.open_qty(), Quantity::ZERO);
}

#[test]
fn test_apply_fill_to_bracket_closes_on_final_target_leg() {
    let mut bracket = long_bracket();
    bracket
        .apply_fill_to_bracket(
            &fill(fixed_uuid(1), "1000", 300, OrderSide::Buy, t1()),
            t1(),
        )
        .expect("entry routed");

    // Exit all three ladder legs; the bracket closes only on the final one.
    for (uuid, price, at) in [
        (11u128, "1100", t2()),
        (12u128, "1150", t3()),
        (13u128, "1200", t4()),
    ] {
        bracket
            .apply_fill_to_bracket(&fill(fixed_uuid(uuid), price, 100, OrderSide::Sell, at), at)
            .expect("target leg routed");
    }
    assert!(bracket.is_closed());
    assert_eq!(bracket.open_qty(), Quantity::ZERO);
    assert!(bracket.remaining_targets.is_empty());

    // Golden PnL: (1100−1000)·100 + (1150−1000)·100 + (1200−1000)·100 = 45,000.
    let golden_pnl = (Decimal::from(1100) - Decimal::from(1000)) * Decimal::from(100)
        + (Decimal::from(1150) - Decimal::from(1000)) * Decimal::from(100)
        + (Decimal::from(1200) - Decimal::from(1000)) * Decimal::from(100);
    let closed = bracket
        .events()
        .iter()
        .find_map(|ev| match ev {
            BracketEvent::Closed {
                realized_pnl,
                reason,
                ..
            } => {
                assert_eq!(reason, "all_targets_exited");
                Some(*realized_pnl)
            }
            _ => None,
        })
        .expect("Closed event journaled");
    assert_eq!(closed, golden_pnl);
    assert_eq!(closed, Decimal::from(45_000));
}

// ————————————————————————————————————————————————————————————
// Entry-fill guards
// ————————————————————————————————————————————————————————————

#[test]
fn test_duplicate_entry_fill_rejected() {
    let mut bracket = long_bracket();
    bracket
        .on_entry_fill(
            &fill(fixed_uuid(1), "1000", 300, OrderSide::Buy, t1()),
            t1(),
        )
        .expect("entry fill ok");

    let err = bracket
        .on_entry_fill(
            &fill(fixed_uuid(1), "1000", 300, OrderSide::Buy, t2()),
            t2(),
        )
        .expect_err("duplicate entry fill rejected");
    assert!(matches!(err, OrderError::InvalidBracket(_)));
    assert_eq!(bracket.open_qty(), Quantity::from_u64(300));
    assert_eq!(bracket.events().len(), 1); // no double-counted EntryFilled event
}

#[test]
fn test_entry_fill_quantity_mismatch_rejected() {
    let mut bracket = long_bracket();
    let err = bracket
        .on_entry_fill(
            &fill(fixed_uuid(1), "1000", 299, OrderSide::Buy, t1()),
            t1(),
        )
        .expect_err("mismatched entry qty rejected");
    assert!(matches!(err, OrderError::InvalidBracket(_)));
    assert_eq!(bracket.open_qty(), Quantity::ZERO);
}
