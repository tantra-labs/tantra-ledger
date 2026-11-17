//! Unit tests for `tantra_ledger_oms::trade_manager` (Plan 0005, P2).

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use tantra_core::{
    Bar, BreakevenTrigger, EntryRule, Exchange, InstrumentId, MaKind, Money, OrderSide, PlanError,
    PlanSubject, PlannerInputs, Quantity, RiskBudget, SizingMethod, SizingSpec, StopRule,
    TargetLadder, TargetLeg, TargetTrigger, TimeStop, Timeframe, TradePlan, TradePlanSpec,
    TradePlanner, TrailRule, TrailSpec,
};
use tantra_ledger_oms::{TradeAction, TradeManager, TradeManagerEvent, TradeSnapshot};

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn ts() -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).unwrap()
}

fn price(s: &str) -> tantra_core::Price {
    tantra_core::Price::from_rupees(s).unwrap()
}

fn instrument() -> InstrumentId {
    InstrumentId::new(Exchange::Nse, "RELIANCE")
}

fn budget() -> RiskBudget {
    RiskBudget::new(
        Money::inr(dec("100000.00")),
        dec("0.01"),
        Quantity::from_u64(1),
    )
}

fn base_inputs() -> PlannerInputs {
    PlannerInputs {
        subject: PlanSubject::Direct,
        instrument: instrument(),
        side: OrderSide::Buy,
        last_price: price("1000.00"),
        tick_size: price("0.05"),
        atr: Some(dec("10.00")),
        adr_pct: None,
        swing_low: None,
        swing_high: None,
        day_low: None,
        day_high: None,
        ma_values: vec![],
        adv20: None,
        level_map: None,
        timestamp: ts(),
    }
}

/// A trailing spec with no trailing rule, no breakeven, no time stop.
fn off_trail() -> TrailSpec {
    TrailSpec {
        rule: TrailRule::Off,
        activation_after_r: Decimal::ONE,
        breakeven_after: None,
        time_stop: None,
    }
}

/// Build a `Direct`-subject market plan with a fixed stop and given ladder /
/// trail. `side` Sell flips the inputs.
fn plan(side: OrderSide, stop: &str, trail: TrailSpec, targets: TargetLadder) -> TradePlan {
    let spec = TradePlanSpec::new(
        "test",
        1,
        EntryRule::Market,
        StopRule::Fixed(price(stop)),
        trail,
        targets,
        SizingSpec {
            method: SizingMethod::FixedFractional {
                risk_pct: dec("0.01"),
            },
            max_weight_pct: None,
            max_adv_pct: None,
        },
    );
    let mut inputs = base_inputs();
    inputs.side = side;
    TradePlanner::plan(&spec, &inputs, &budget()).unwrap()
}

/// Two-leg ladder: ½ @ 2R then ½ @ 3R.
fn ladder() -> TargetLadder {
    TargetLadder::new(
        vec![
            TargetLeg {
                trigger: TargetTrigger::AtR(Decimal::from(2)),
                fraction: dec("0.5"),
            },
            TargetLeg {
                trigger: TargetTrigger::AtR(Decimal::from(3)),
                fraction: dec("0.5"),
            },
        ],
        true,
    )
    .unwrap()
}

fn bar(open: &str, high: &str, low: &str, close: &str) -> Bar {
    Bar {
        instrument: instrument(),
        open: price(open),
        high: price(high),
        low: price(low),
        close: price(close),
        volume: Quantity::from_u64(0),
        timestamp: ts(),
    }
}

fn snapshot(last: &str, atr: Option<&str>) -> TradeSnapshot {
    TradeSnapshot {
        last_price: price(last),
        atr: atr.map(dec),
        ma_values: vec![],
        high: None,
        low: None,
        timestamp: ts(),
    }
}

#[test]
fn entry_fill_reanchors_risk_and_target_ladder() {
    let plan = plan(OrderSide::Buy, "980.00", off_trail(), ladder());
    let mut manager = TradeManager::new(&plan).unwrap();

    // Carries the planned state before any fill.
    assert_eq!(manager.open_quantity(), Quantity::from_u64(50));
    assert_eq!(manager.stop(), price("980.00"));
    assert_eq!(manager.risk_per_unit(), dec("20.00"));
    assert!(!manager.is_closed());
    assert_eq!(manager.fill_price(), None);

    // Better fill: risk re-anchors to |1010 - 980| = 30, targets re-price.
    let actions = manager
        .on_entry_fill(price("1010.00"), Quantity::from_u64(50), ts())
        .unwrap();
    assert!(actions.is_empty());
    assert_eq!(manager.risk_per_unit(), dec("30.00"));
    assert_eq!(manager.fill_price(), Some(price("1010.00")));
    assert_eq!(manager.remaining_targets().len(), 2);
    // 2R leg → 1010 + 2×30 = 1070; 3R leg → 1010 + 3×30 = 1100.
    assert_eq!(manager.remaining_targets()[0].price, price("1070.00"));
    assert_eq!(manager.remaining_targets()[0].r, Decimal::from(2));
    assert_eq!(manager.remaining_targets()[1].price, price("1100.00"));
    assert_eq!(
        manager.events(),
        &[TradeManagerEvent::EntryFilled {
            ts: ts(),
            fill_price: price("1010.00"),
            qty: Quantity::from_u64(50),
        }]
    );

    // 2R leg fills intrabar.
    let actions = manager.on_bar(
        &bar("1050.00", "1075.00", "1040.00", "1070.00"),
        &snapshot("1070.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::PartialExit {
            qty: Quantity::from_u64(25),
            reason: "target at 2R".to_string(),
        }]
    );
    assert_eq!(manager.open_quantity(), Quantity::from_u64(25));
    assert_eq!(manager.realized_pnl(), dec("1500.00"));

    // Stop hit later at 980 closes the runner.
    let actions = manager.on_bar(
        &bar("1000.00", "1010.00", "975.00", "985.00"),
        &snapshot("985.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::ExitAll {
            reason: "stop".to_string()
        }]
    );
    assert_eq!(manager.realized_pnl(), dec("750.00"));
    assert!(manager.is_closed());
    assert_eq!(manager.open_quantity(), Quantity::ZERO);
}

#[test]
fn entry_fill_gap_through_stop_fails_closed() {
    // Long: fill below the stop collapses the risk budget.
    let long_plan = plan(OrderSide::Buy, "980.00", off_trail(), ladder());
    let mut manager = TradeManager::new(&long_plan).unwrap();
    let err = manager
        .on_entry_fill(price("970.00"), Quantity::from_u64(50), ts())
        .unwrap_err();
    assert_eq!(err, PlanError::ZeroRisk);
    // No partial state: still unfilled, nothing journaled, position untouched.
    assert_eq!(manager.fill_price(), None);
    assert_eq!(manager.open_quantity(), Quantity::from_u64(50));
    assert!(manager.events().is_empty());
    assert!(!manager.is_closed());

    // Short: fill above the stop collapses risk too.
    let short_plan = plan(OrderSide::Sell, "1020.00", off_trail(), ladder());
    let mut manager = TradeManager::new(&short_plan).unwrap();
    let err = manager
        .on_entry_fill(price("1030.00"), Quantity::from_u64(50), ts())
        .unwrap_err();
    assert_eq!(err, PlanError::ZeroRisk);
    assert_eq!(manager.fill_price(), None);

    // A fill exactly on the stop is also a collapsed-risk refusal.
    let exact_plan = plan(OrderSide::Buy, "980.00", off_trail(), ladder());
    let mut manager = TradeManager::new(&exact_plan).unwrap();
    let err = manager
        .on_entry_fill(price("980.00"), Quantity::from_u64(50), ts())
        .unwrap_err();
    assert_eq!(err, PlanError::ZeroRisk);
}

#[test]
fn short_entry_fill_reanchors_targets_below_fill() {
    let plan = plan(OrderSide::Sell, "1020.00", off_trail(), ladder());
    let mut manager = TradeManager::new(&plan).unwrap();
    // Fill improves: 990 → risk 30, legs re-price below the fill.
    manager
        .on_entry_fill(price("990.00"), Quantity::from_u64(50), ts())
        .unwrap();
    assert_eq!(manager.risk_per_unit(), dec("30.00"));
    assert_eq!(manager.remaining_targets()[0].price, price("930.00"));
    assert_eq!(manager.remaining_targets()[1].price, price("900.00"));

    // Both legs fill when the bar lows pierce them.
    let actions = manager.on_bar(
        &bar("980.00", "995.00", "895.00", "910.00"),
        &snapshot("910.00", None),
    );
    assert_eq!(
        actions,
        vec![
            TradeAction::PartialExit {
                qty: Quantity::from_u64(25),
                reason: "target at 2R".to_string(),
            },
            TradeAction::PartialExit {
                qty: Quantity::from_u64(25),
                reason: "target at 3R".to_string(),
            },
        ]
    );
    assert!(manager.is_closed());
    assert_eq!(manager.realized_pnl(), dec("3750.00")); // 25×60 + 25×90
}

#[test]
fn breakeven_after_r_moves_stop_to_fill() {
    let trail = TrailSpec {
        rule: TrailRule::Off,
        activation_after_r: Decimal::ONE,
        breakeven_after: Some(BreakevenTrigger::AtR(Decimal::ONE)),
        time_stop: None,
    };
    let plan = plan(OrderSide::Buy, "980.00", trail, ladder());
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(50), ts())
        .unwrap();

    // r = 1.0 at last 1020 → breakeven trigger fires → stop moves to the fill.
    let actions = manager.on_bar(
        &bar("1010.00", "1025.00", "1005.00", "1020.00"),
        &snapshot("1020.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::MoveStop {
            new: price("1000.00")
        }]
    );
    assert_eq!(manager.stop(), price("1000.00"));
    // Monotone: a later move can never loosen it back to 980.
    assert!(manager.stop() > price("980.00"));
}

#[test]
fn breakeven_after_first_target_fires_next_bar() {
    let trail = TrailSpec {
        rule: TrailRule::Off,
        activation_after_r: Decimal::ONE,
        breakeven_after: Some(BreakevenTrigger::AfterFirstTarget),
        time_stop: None,
    };
    let plan = plan(OrderSide::Buy, "980.00", trail, ladder());
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(50), ts())
        .unwrap();

    // First target fills (high pierces 1040): breakeven fires on the next bar.
    let actions = manager.on_bar(
        &bar("1010.00", "1045.00", "1005.00", "1040.00"),
        &snapshot("1040.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::PartialExit {
            qty: Quantity::from_u64(25),
            reason: "target at 2R".to_string(),
        }]
    );
    assert_eq!(manager.stop(), price("980.00"));

    let actions = manager.on_bar(
        &bar("1020.00", "1030.00", "1010.00", "1025.00"),
        &snapshot("1025.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::MoveStop {
            new: price("1000.00")
        }]
    );
    assert_eq!(manager.stop(), price("1000.00"));
}

#[test]
fn breakeven_after_sessions_fires_at_session_close() {
    let trail = TrailSpec {
        rule: TrailRule::Off,
        activation_after_r: Decimal::ONE,
        breakeven_after: Some(BreakevenTrigger::AfterSessions(2)),
        time_stop: None,
    };
    let plan = plan(OrderSide::Buy, "980.00", trail, ladder());
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(50), ts())
        .unwrap();

    // Session 1: not yet at breakeven.
    let actions = manager.on_session_close(
        Timeframe::M5,
        &bar("1000.00", "1000.00", "1000.00", "1000.00"),
        &snapshot("1000.00", None),
    );
    assert!(actions.is_empty());
    assert_eq!(manager.stop(), price("980.00"));

    // Session 2: the stop moves to the fill at the session close.
    let actions = manager.on_session_close(
        Timeframe::M5,
        &bar("1000.00", "1000.00", "1000.00", "1000.00"),
        &snapshot("1000.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::MoveStop {
            new: price("1000.00")
        }]
    );
    assert_eq!(manager.stop(), price("1000.00"));
}

#[test]
fn atr_trail_tightens_monotone_long() {
    let trail = TrailSpec {
        rule: TrailRule::Atr {
            mult: Decimal::from(3),
        },
        activation_after_r: Decimal::ONE,
        breakeven_after: None,
        time_stop: None,
    };
    let plan = plan(
        OrderSide::Buy,
        "980.00",
        trail,
        TargetLadder::new(vec![], true).unwrap(),
    );
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(50), ts())
        .unwrap();

    // 1.25R → trail active; high 1030 − 3×10 = 1000.
    let actions = manager.on_bar(
        &bar("1010.00", "1030.00", "1010.00", "1025.00"),
        &TradeSnapshot {
            last_price: price("1025.00"),
            atr: Some(dec("10.00")),
            ma_values: vec![],
            high: Some(price("1030.00")),
            low: Some(price("1010.00")),
            timestamp: ts(),
        },
    );
    assert_eq!(
        actions,
        vec![TradeAction::MoveStop {
            new: price("1000.00")
        }]
    );
    assert_eq!(manager.stop(), price("1000.00"));

    // New extreme 1040 − 30 = 1010.
    let actions = manager.on_bar(
        &bar("1020.00", "1040.00", "1020.00", "1035.00"),
        &TradeSnapshot {
            last_price: price("1035.00"),
            atr: Some(dec("10.00")),
            ma_values: vec![],
            high: Some(price("1040.00")),
            low: Some(price("1020.00")),
            timestamp: ts(),
        },
    );
    assert_eq!(
        actions,
        vec![TradeAction::MoveStop {
            new: price("1010.00")
        }]
    );
    assert_eq!(manager.stop(), price("1010.00"));

    // Falling bar: the candidate 1000 would loosen → no move, no action.
    let actions = manager.on_bar(
        &bar("1020.00", "1030.00", "1015.00", "1025.00"),
        &TradeSnapshot {
            last_price: price("1025.00"),
            atr: Some(dec("10.00")),
            ma_values: vec![],
            high: Some(price("1030.00")),
            low: Some(price("1015.00")),
            timestamp: ts(),
        },
    );
    assert!(actions.is_empty());
    assert_eq!(manager.stop(), price("1010.00"));
}

#[test]
fn atr_trail_tightens_monotone_short() {
    let trail = TrailSpec {
        rule: TrailRule::Atr {
            mult: Decimal::from(3),
        },
        activation_after_r: Decimal::ONE,
        breakeven_after: None,
        time_stop: None,
    };
    let plan = plan(
        OrderSide::Sell,
        "1020.00",
        trail,
        TargetLadder::new(vec![], true).unwrap(),
    );
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(50), ts())
        .unwrap();

    // 0.5R: trail not yet active.
    let actions = manager.on_bar(
        &bar("990.00", "1000.00", "985.00", "990.00"),
        &snapshot("990.00", Some("10.00")),
    );
    assert!(actions.is_empty());
    assert_eq!(manager.stop(), price("1020.00"));

    // 1.0R: low 975 + 30 = 1005 → tighten.
    let actions = manager.on_bar(
        &bar("985.00", "995.00", "975.00", "980.00"),
        &snapshot("980.00", Some("10.00")),
    );
    assert_eq!(
        actions,
        vec![TradeAction::MoveStop {
            new: price("1005.00")
        }]
    );
    assert_eq!(manager.stop(), price("1005.00"));

    // Bounce: candidate 980 + 30 = 1010 would loosen → no move.
    let actions = manager.on_bar(
        &bar("985.00", "990.00", "980.00", "985.00"),
        &snapshot("985.00", Some("10.00")),
    );
    assert!(actions.is_empty());
    assert_eq!(manager.stop(), price("1005.00"));
}

#[test]
fn target_ladder_partials_clamp_to_open_quantity() {
    // Plan sizes 50; the fill is only 40. Leg 2 (25) must clamp to the 15 left
    // after leg 1 — the position never flips short.
    let plan = plan(OrderSide::Buy, "980.00", off_trail(), ladder());
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(40), ts())
        .unwrap();

    let actions = manager.on_bar(
        &bar("1010.00", "1070.00", "1000.00", "1060.00"),
        &snapshot("1060.00", None),
    );
    assert_eq!(
        actions,
        vec![
            TradeAction::PartialExit {
                qty: Quantity::from_u64(25),
                reason: "target at 2R".to_string(),
            },
            TradeAction::PartialExit {
                qty: Quantity::from_u64(15),
                reason: "target at 3R".to_string(),
            },
        ]
    );
    assert_eq!(manager.open_quantity(), Quantity::ZERO);
    assert!(manager.is_closed());
    assert_eq!(
        manager.realized_pnl(),
        dec("40.00") * dec("25") + dec("60.00") * dec("15")
    );
}

#[test]
fn stop_wins_when_both_stop_and_target_inside_bar() {
    // Conservative intrabar policy: stop (980) and 2R target (1040) are both
    // inside the bar → the stop wins, no partial fills.
    let plan = plan(OrderSide::Buy, "980.00", off_trail(), ladder());
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(50), ts())
        .unwrap();

    let actions = manager.on_bar(
        &bar("1010.00", "1050.00", "975.00", "1020.00"),
        &snapshot("1020.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::ExitAll {
            reason: "stop".to_string()
        }]
    );
    assert!(manager.is_closed());
    assert_eq!(manager.realized_pnl(), dec("-1000.00"));
    assert_eq!(
        manager.events(),
        &[
            TradeManagerEvent::EntryFilled {
                ts: ts(),
                fill_price: price("1000.00"),
                qty: Quantity::from_u64(50),
            },
            TradeManagerEvent::ExitAll {
                ts: ts(),
                qty: Quantity::from_u64(50),
                price: price("980.00"),
                reason: "stop".to_string(),
            },
        ]
    );
}

#[test]
fn time_stop_exits_after_bars_without_progress() {
    let trail = TrailSpec {
        rule: TrailRule::Off,
        activation_after_r: Decimal::ONE,
        breakeven_after: None,
        time_stop: Some(TimeStop {
            bars: 3,
            min_progress_r: Decimal::ONE,
            timeframe: Timeframe::M5,
        }),
    };
    let plan = plan(
        OrderSide::Buy,
        "980.00",
        trail,
        TargetLadder::new(vec![], false).unwrap(),
    );
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(50), ts())
        .unwrap();

    // Two flat sessions: no exit yet.
    for _ in 0..2 {
        let actions = manager.on_session_close(
            Timeframe::M5,
            &bar("1000.00", "1000.00", "1000.00", "1000.00"),
            &snapshot("1000.00", None),
        );
        assert!(actions.is_empty());
        assert!(!manager.is_closed());
    }

    // Third flat session (0R < 1R after 3 bars) → exit scheduled, executed next bar.
    let actions = manager.on_session_close(
        Timeframe::M5,
        &bar("1000.00", "1000.00", "1000.00", "1000.00"),
        &snapshot("1000.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::ExitAll {
            reason: "time_stop".to_string()
        }]
    );
    // No look-ahead: still open until the next bar.
    assert!(!manager.is_closed());
    assert_eq!(manager.open_quantity(), Quantity::from_u64(50));

    // Next bar: the exit executes at its open.
    let actions = manager.on_bar(
        &bar("1005.00", "1008.00", "1002.00", "1007.00"),
        &snapshot("1007.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::ExitAll {
            reason: "time_stop".to_string()
        }]
    );
    assert!(manager.is_closed());
    assert_eq!(manager.realized_pnl(), dec("250.00"));
}

#[test]
fn close_ma_session_exit_has_no_look_ahead() {
    let trail = TrailSpec {
        rule: TrailRule::CloseMa {
            kind: MaKind::Sma,
            period: 10,
        },
        activation_after_r: Decimal::ONE,
        breakeven_after: None,
        time_stop: None,
    };
    let plan = plan(
        OrderSide::Buy,
        "980.00",
        trail,
        TargetLadder::new(vec![], false).unwrap(),
    );
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(50), ts())
        .unwrap();

    // Close 1020 (1R, trail active) below SMA10 1025 → exit signaled at close.
    let mut snap = snapshot("1020.00", None);
    snap.ma_values = vec![(MaKind::Sma, 10, dec("1025.00"))];
    let actions = manager.on_session_close(
        Timeframe::D1,
        &bar("1000.00", "1020.00", "1000.00", "1020.00"),
        &snap,
    );
    assert_eq!(
        actions,
        vec![TradeAction::ExitAll {
            reason: "close_ma".to_string()
        }]
    );
    // No look-ahead: the close-rule exit does NOT fill on the same session.
    assert!(!manager.is_closed());
    assert_eq!(manager.open_quantity(), Quantity::from_u64(50));

    // It executes on the next session's open.
    let actions = manager.on_bar(
        &bar("1015.00", "1030.00", "1010.00", "1025.00"),
        &snapshot("1025.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::ExitAll {
            reason: "close_ma".to_string()
        }]
    );
    assert!(manager.is_closed());
    assert_eq!(manager.realized_pnl(), dec("750.00"));
    assert_eq!(
        manager.events(),
        &[
            TradeManagerEvent::EntryFilled {
                ts: ts(),
                fill_price: price("1000.00"),
                qty: Quantity::from_u64(50),
            },
            TradeManagerEvent::ExitAll {
                ts: ts(),
                qty: Quantity::from_u64(50),
                price: price("1015.00"),
                reason: "close_ma".to_string(),
            },
        ]
    );
}

#[test]
fn after_sessions_target_leg_fires_at_session_close() {
    let trail = off_trail();
    let targets = TargetLadder::new(
        vec![TargetLeg {
            trigger: TargetTrigger::AfterSessions(2),
            fraction: Decimal::ONE,
        }],
        false,
    )
    .unwrap();
    let plan = plan(OrderSide::Buy, "980.00", trail, targets);
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1000.00"), Quantity::from_u64(50), ts())
        .unwrap();

    // Session 1: not yet.
    let actions = manager.on_session_close(
        Timeframe::M5,
        &bar("1005.00", "1008.00", "1002.00", "1005.00"),
        &snapshot("1005.00", None),
    );
    assert!(actions.is_empty());

    // Session 2: the time leg fires as a pending partial.
    let actions = manager.on_session_close(
        Timeframe::M5,
        &bar("1005.00", "1008.00", "1002.00", "1005.00"),
        &snapshot("1005.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::PartialExit {
            qty: Quantity::from_u64(50),
            reason: "time exit after 2 session(s)".to_string(),
        }]
    );
    assert!(!manager.is_closed());

    // Executes at the next bar's open.
    let actions = manager.on_bar(
        &bar("1010.00", "1015.00", "1008.00", "1012.00"),
        &snapshot("1012.00", None),
    );
    assert_eq!(
        actions,
        vec![TradeAction::PartialExit {
            qty: Quantity::from_u64(50),
            reason: "time exit after 2 session(s)".to_string(),
        }]
    );
    assert!(manager.is_closed());
    assert_eq!(manager.realized_pnl(), dec("500.00"));
}

#[test]
fn trade_manager_state_serde_roundtrip() {
    let trail = TrailSpec {
        rule: TrailRule::Atr {
            mult: Decimal::from(3),
        },
        activation_after_r: Decimal::ONE,
        breakeven_after: Some(BreakevenTrigger::AfterFirstTarget),
        time_stop: None,
    };
    let plan = plan(OrderSide::Buy, "980.00", trail, ladder());
    let mut manager = TradeManager::new(&plan).unwrap();
    manager
        .on_entry_fill(price("1010.00"), Quantity::from_u64(50), ts())
        .unwrap();
    manager.on_bar(
        &bar("1050.00", "1075.00", "1040.00", "1070.00"),
        &snapshot("1070.00", Some("10.00")),
    );

    // Persist → resume: the state round-trips exactly.
    let json = serde_json::to_string(&manager).unwrap();
    let resumed: TradeManager = serde_json::from_str(&json).unwrap();
    assert_eq!(resumed, manager);
    assert_eq!(resumed.open_quantity(), manager.open_quantity());
    assert_eq!(resumed.remaining_targets(), manager.remaining_targets());
    assert_eq!(resumed.events(), manager.events());
}
