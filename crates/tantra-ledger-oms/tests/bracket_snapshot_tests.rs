//! `OmsSnapshot.open_brackets` surface tests (gap 0012 §4).
//!
//! The snapshot's `open_brackets` list reflects an open bracket aggregate and
//! clears once the bracket closes; the field defaults to empty (serde) and to
//! `OmsSnapshot::default()`.

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use tantra_core::{
    Exchange, FillReport, InstrumentId, OrderRequest, OrderSide, OrderType, PlannedTarget, Price,
    Quantity, TrailRule,
};
use tantra_ledger_oms::{BracketOrder, OmsSnapshot};
use uuid::Uuid;

fn t1() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 9, 30, 0).unwrap()
}

fn t2() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 10, 0, 0).unwrap()
}

fn fixed_uuid(n: u128) -> Uuid {
    Uuid::from_u128(0x2026_0000_0000_0000 + n)
}

fn reliance() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "RELIANCE").unwrap()
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

/// Open long bracket: entry 1000 × 300 filled, stop moved to 1020 with a
/// resized stop quantity of 200 (one ⅓ target already exited).
fn open_long_bracket() -> BracketOrder {
    let mut bracket = BracketOrder::new(
        OrderRequest::with_id(
            fixed_uuid(1),
            reliance(),
            OrderSide::Buy,
            OrderType::Limit(Price::from_rupees("1000").unwrap()),
            Quantity::from_u64(300),
            "EE_BRACKET",
        ),
        Price::from_rupees("950").unwrap(),
        vec![
            PlannedTarget {
                price: Price::from_rupees("1100").unwrap(),
                qty: Quantity::from_u64(100),
                reason: "target 1 at 2R".to_string(),
            },
            PlannedTarget {
                price: Price::from_rupees("1150").unwrap(),
                qty: Quantity::from_u64(100),
                reason: "target 2 at 3R".to_string(),
            },
            PlannedTarget {
                price: Price::from_rupees("1200").unwrap(),
                qty: Quantity::from_u64(100),
                reason: "target 3 at 4R".to_string(),
            },
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
            Price::from_rupees("1020").unwrap(),
            Some(Quantity::from_u64(200)),
            t2(),
        )
        .expect("stop update ok");
    bracket
}

#[test]
fn test_default_snapshot_has_no_open_brackets() {
    let snap = OmsSnapshot::default();
    assert!(snap.open_brackets.is_empty());
    assert_eq!(snap.total_orders, 0);
}

#[test]
fn test_snapshot_reflects_open_bracket_and_clears_once_closed() {
    let bracket = open_long_bracket();
    assert!(!bracket.is_closed());

    // The OMS surface carries the open bracket as a BracketView.
    let mut open_brackets = vec![bracket.view()];
    let snap = OmsSnapshot {
        open_brackets: open_brackets.clone(),
        ..OmsSnapshot::default()
    };
    assert_eq!(snap.open_brackets.len(), 1);

    let view = &snap.open_brackets[0];
    assert_eq!(view.bracket_id, bracket.bracket_id);
    assert_eq!(view.entry_price, Some(Price::from_rupees("1000").unwrap()));
    assert_eq!(view.resting_stop, Price::from_rupees("1020").unwrap());
    assert_eq!(view.resting_stop_qty, Quantity::from_u64(200));
    assert_eq!(view.open_qty, Quantity::from_u64(200));
    assert_eq!(view.target_count, 2); // one of three ladder legs already exited
    assert!(view.tsl_active); // trailing rule configured

    // Stop trigger on the runner closes the bracket; the surface clears it.
    let mut bracket = bracket;
    bracket
        .on_stop_trigger(Price::from_rupees("1020").unwrap(), t2())
        .expect("stop trigger ok");
    assert!(bracket.is_closed());
    assert_eq!(bracket.open_qty(), Quantity::ZERO);

    open_brackets.retain(|v| v.bracket_id != bracket.bracket_id);
    let snap_after = OmsSnapshot {
        open_brackets,
        ..OmsSnapshot::default()
    };
    assert!(snap_after.open_brackets.is_empty());
}

#[test]
fn test_snapshot_with_bracket_roundtrips_through_json() {
    let bracket = open_long_bracket();
    let snap = OmsSnapshot {
        open_brackets: vec![bracket.view()],
        ..OmsSnapshot::default()
    };
    let json = serde_json::to_string(&snap).expect("serialize snapshot");
    let back: OmsSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");
    assert_eq!(back, snap);
    assert_eq!(back.open_brackets.len(), 1);
    assert_eq!(back.open_brackets[0].bracket_id, bracket.bracket_id);
}

#[test]
fn test_snapshot_deserializes_without_open_brackets_field() {
    // Strip the open_brackets key — the serde(default) field must default to
    // an empty vec so old wire payloads keep deserializing.
    let mut value = serde_json::to_value(OmsSnapshot::default()).expect("serialize snapshot");
    assert!(
        value
            .as_object_mut()
            .expect("object")
            .remove("open_brackets")
            .is_some()
    );
    let json = serde_json::to_string(&value).expect("stringify");
    let snap: OmsSnapshot = serde_json::from_str(&json).expect("deserialize legacy snapshot");
    assert!(snap.open_brackets.is_empty());
}
