//! Bracket-order invariant proptests (gap 0012).
//!
//! After *any* sequence of partial exits and stop updates the bracket's
//! `open_qty` stays non-negative (no short flip) and the resting stop never
//! widens — a long stop only moves up or holds, a short stop only moves down
//! or holds (mirrors `tantra-risk` gap 0005). Invalid operations are rejected
//! without mutating the aggregate, so the invariants must hold whether each
//! operation is accepted or refused.

use chrono::{DateTime, TimeZone, Utc};
use proptest::prelude::*;
use rust_decimal::Decimal;
use tantra_core::{
    Exchange, FillReport, InstrumentId, OrderRequest, OrderSide, OrderType, PlannedTarget, Price,
    Quantity, TrailRule,
};
use tantra_ledger_oms::BracketOrder;
use uuid::Uuid;

const ENTRY_QTY: u64 = 3000;
const ENTRY_PRICE: &str = "1000";
const INITIAL_STOP: &str = "900"; // long stop below the entry
const SHORT_STOP: &str = "1100"; // short stop above the entry

fn fixed_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 9, 15, 0).unwrap()
}

fn reliance() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "RELIANCE").unwrap()
}

/// Randomised operation fed to the aggregate. Operations may be invalid for
/// the current state (over-exit, stop widening, oversized stop qty); those
/// must be refused without mutating state.
#[derive(Debug, Clone)]
enum BracketOp {
    /// A partial exit of `qty` shares.
    PartialExit(u64),
    /// A stop update to `stop_offset` rupees relative to the initial stop,
    /// with an optional new stop quantity.
    StopUpdate(i64, Option<u64>),
}

impl Arbitrary for BracketOp {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop_oneof![
            (1u64..20_000u64).prop_map(BracketOp::PartialExit),
            ((-800i64..800i64), prop::option::of(0u64..20_000u64))
                .prop_map(|(offset, qty)| BracketOp::StopUpdate(offset, qty)),
        ]
        .boxed()
    }
}

fn bracket_for(side: OrderSide) -> BracketOrder {
    let (stop, targets) = match side {
        OrderSide::Buy => (
            INITIAL_STOP,
            vec![PlannedTarget {
                price: Price::from_rupees("1200").unwrap(),
                qty: Quantity::from_u64(1000),
                reason: "target 1".to_string(),
            }],
        ),
        OrderSide::Sell => (
            SHORT_STOP,
            vec![PlannedTarget {
                price: Price::from_rupees("800").unwrap(),
                qty: Quantity::from_u64(1000),
                reason: "target 1".to_string(),
            }],
        ),
    };
    let mut bracket = BracketOrder::new(
        OrderRequest::with_id(
            Uuid::from_u128(0x2026_0000_0000_0000 + 0xFFFF),
            reliance(),
            side,
            OrderType::Limit(Price::from_rupees(ENTRY_PRICE).unwrap()),
            Quantity::from_u64(ENTRY_QTY),
            "EE_BRACKET",
        ),
        Price::from_rupees(stop).unwrap(),
        targets,
        Some(TrailRule::Percent(Decimal::from(2))),
    )
    .expect("valid bracket");
    bracket
        .on_entry_fill(
            &FillReport::new(
                "fill-entry",
                "order-entry",
                Uuid::from_u128(0x2026_0000_0000_0000 + 0xFFFF),
                reliance(),
                side,
                Price::from_rupees(ENTRY_PRICE).unwrap(),
                Quantity::from_u64(ENTRY_QTY),
                "EE_BRACKET",
                fixed_time(),
                "acct-1",
            ),
            fixed_time(),
        )
        .expect("entry fill ok");
    bracket
}

fn apply_op(bracket: &mut BracketOrder, op: &BracketOp, now: DateTime<Utc>) {
    match *op {
        BracketOp::PartialExit(qty) => {
            let result = bracket.on_partial_exit(
                Price::from_rupees("1010").unwrap(),
                Quantity::from_u64(qty),
                "proptest",
                now,
            );
            let _ = result; // invalid exits (short flip) are refused — invariant below.
        }
        BracketOp::StopUpdate(offset, qty) => {
            let base: i64 = if bracket.entry.side == OrderSide::Buy {
                INITIAL_STOP.parse().expect("parse stop")
            } else {
                SHORT_STOP.parse().expect("parse stop")
            };
            let new_stop = Price::from_rupees(&(base + offset).to_string()).expect("valid price");
            let result = bracket.apply_stop_update(new_stop, qty.map(Quantity::from_u64), now);
            let _ = result; // widening / oversized qty refused — invariant below.
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn open_qty_nonnegative_and_stop_never_widens(
        side in any::<bool>(),
        ops in prop::collection::vec(any::<BracketOp>(), 0..60),
    ) {
        let side = if side { OrderSide::Buy } else { OrderSide::Sell };
        let mut bracket = bracket_for(side);
        let mut last_stop = bracket.resting_stop();

        for (i, op) in ops.iter().enumerate() {
            let before_qty = bracket.open_qty();
            let before_stop = bracket.resting_stop();

            apply_op(&mut bracket, op, fixed_time());

            // Invariant 1: open quantity can never go negative (no short flip).
            prop_assert!(
                bracket.open_qty().value() >= Decimal::ZERO,
                "open_qty went negative at op {i}: {op:?}"
            );

            // Invariant 2: the stop never widens — a long stop only moves up or
            // holds, a short stop only moves down or holds.
            let step = match side {
                OrderSide::Buy => bracket.resting_stop().value() >= before_stop.value(),
                OrderSide::Sell => bracket.resting_stop().value() <= before_stop.value(),
            };
            prop_assert!(step, "stop widened at op {i}: {op:?} ({before_stop} -> {})", bracket.resting_stop());

            // Monotonic across the whole sequence (widening ops are refused
            // without mutating the resting stop).
            let monotonic = match side {
                OrderSide::Buy => bracket.resting_stop().value() >= last_stop.value(),
                OrderSide::Sell => bracket.resting_stop().value() <= last_stop.value(),
            };
            prop_assert!(monotonic, "resting stop regressed at op {i}");
            last_stop = bracket.resting_stop();

            // Rejected operations must leave the aggregate untouched.
            let accepted_partial = match op {
                BracketOp::PartialExit(q) => {
                    *q <= ENTRY_QTY && !bracket.closed && before_qty.value() >= Decimal::from(*q)
                }
                _ => false,
            };
            if !accepted_partial {
                prop_assert_eq!(
                    bracket.open_qty(),
                    before_qty,
                    "open_qty mutated by op {}: {:?}",
                    i,
                    op
                );
            }
        }

        // After entry fill, open_qty + sum(partial exits) == entry qty (no
        // double count) whenever the bracket is still open or closed via exits.
        let partial_exits: Decimal = bracket
            .events()
            .iter()
            .filter_map(|ev| match ev {
                tantra_ledger_oms::BracketEvent::PartialExit { qty, .. } => Some(qty.value()),
                _ => None,
            })
            .sum();
        prop_assert!(
            bracket.open_qty().value() + partial_exits <= Decimal::from(ENTRY_QTY),
            "double-counted exits: open {} + partials {} > entry {ENTRY_QTY}",
            bracket.open_qty().value(),
            partial_exits,
        );
    }
}
