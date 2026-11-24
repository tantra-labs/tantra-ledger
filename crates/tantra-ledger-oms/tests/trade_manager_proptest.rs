//! Property tests for `tantra_ledger_oms::trade_manager` (Plan 0005, P2).
//!
//! Invariants under random bar walks: the open quantity never goes negative,
//! a long stop never loosens, and realized P&L plus remaining risk never
//! exceeds the position risk committed at the fill.

use chrono::{TimeZone, Utc};
use proptest::prelude::*;
use rust_decimal::Decimal;
use tantra_core::Price;
use tantra_core::{
    Bar, BreakevenTrigger, EntryRule, Exchange, InstrumentId, Money, OrderSide, PlanSubject,
    PlannerInputs, Quantity, RiskBudget, SizingMethod, SizingSpec, StopRule, TargetLadder,
    TargetLeg, TargetTrigger, TradePlan, TradePlanSpec, TradePlanner, TrailRule, TrailSpec,
};
use tantra_ledger_oms::{TradeManager, TradeSnapshot};

fn ts() -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).unwrap()
}

fn instrument() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "RELIANCE").unwrap()
}

/// Long plan with an ATR trail (3×ATR, activation 1R) and an AtR(1) breakeven
/// trigger, so the manager invariant tests exercise real stop moves.
fn plan_long_trailing(
    entry: Decimal,
    tick: Decimal,
    stop: Decimal,
    equity: Decimal,
    risk_pct: Decimal,
    min_lot: u64,
) -> Option<TradePlan> {
    if !(entry > Decimal::ZERO && stop > Decimal::ZERO && stop < entry) {
        return None;
    }
    let spec = TradePlanSpec::new(
        "prop_trail",
        1,
        EntryRule::Market,
        StopRule::Fixed(Price::try_new(stop).unwrap()),
        TrailSpec {
            rule: TrailRule::Atr {
                mult: Decimal::from(3),
            },
            activation_after_r: Decimal::ONE,
            breakeven_after: Some(BreakevenTrigger::AtR(Decimal::ONE)),
            time_stop: None,
        },
        TargetLadder::new(
            vec![TargetLeg {
                trigger: TargetTrigger::AtR(Decimal::from(2)),
                fraction: Decimal::ONE,
            }],
            false,
        )
        .unwrap(),
        SizingSpec {
            method: SizingMethod::FixedFractional { risk_pct },
            max_weight_pct: None,
            max_adv_pct: None,
        },
    );
    let inputs = PlannerInputs {
        subject: PlanSubject::Direct,
        instrument: instrument(),
        side: OrderSide::Buy,
        last_price: Price::try_new(entry).unwrap(),
        tick_size: Price::try_new(tick).unwrap(),
        atr: Some(entry / Decimal::from(100)),
        adr_pct: None,
        swing_low: None,
        swing_high: None,
        day_low: None,
        day_high: None,
        ma_values: vec![],
        adv20: None,
        level_map: None,
        timestamp: ts(),
    };
    let budget = RiskBudget::new(Money::inr(equity), risk_pct, Quantity::from_u64(min_lot));
    TradePlanner::plan(&spec, &inputs, &budget).ok()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn prop_trade_manager_invariants(
        entry in 1_000i64..100_000i64,
        stop_gap in 1i64..=5_000i64,
        equity in 100_000i64..100_000_000i64,
        moves in prop::collection::vec(-2_000i64..2_000i64, 1..60),
    ) {
        let entry_d = Decimal::from(entry);
        let stop_d = Decimal::from(entry - stop_gap);
        let risk_pct = Decimal::from(50) / Decimal::from(10000);
        let Some(plan) = plan_long_trailing(entry_d, Decimal::from(1), stop_d, Decimal::from(equity), risk_pct, 1) else {
            return Ok(());
        };
        let mut manager = TradeManager::new(&plan).unwrap();
        manager.on_entry_fill(plan.entry, plan.quantity, ts()).unwrap();
        let risk_at_stake = plan.risk_at_stake.amount;
        let fill = plan.entry.value();
        let mut last_stop = manager.stop().value();
        let mut price = entry_d;
        for move_value in moves {
            price = (price + Decimal::from(move_value)).max(Decimal::ONE);
            let high = price + Decimal::from(10);
            let low = (price - Decimal::from(10)).max(Decimal::ONE);
            let bar = Bar {
                instrument: instrument(),
                open: Price::try_new(price).unwrap(),
                high: Price::try_new(high).unwrap(),
                low: Price::try_new(low).unwrap(),
                close: Price::try_new(price).unwrap(),
                volume: Quantity::from_u64(0),
                timestamp: ts(),
            };
            let snapshot = TradeSnapshot {
                last_price: Price::try_new(price).unwrap(),
                atr: Some(entry_d / Decimal::from(100)),
                ma_values: vec![],
                high: None,
                low: None,
                timestamp: ts(),
            };
            manager.on_bar(&bar, &snapshot);

            prop_assert!(
                manager.open_quantity().value() >= Decimal::ZERO,
                "open quantity must never go negative"
            );
            let stop = manager.stop().value();
            prop_assert!(
                stop >= last_stop,
                "long stop must never loosen: {} -> {}",
                last_stop,
                stop
            );
            last_stop = stop;

            let remaining_risk =
                manager.open_quantity().value() * (fill - manager.stop().value()).abs();
            prop_assert!(
                manager.realized_pnl() + remaining_risk >= -risk_at_stake,
                "realized {} + remaining risk {} must never exceed position risk {}",
                manager.realized_pnl(),
                remaining_risk,
                -risk_at_stake
            );
            if manager.is_closed() {
                break;
            }
        }
    }
}
