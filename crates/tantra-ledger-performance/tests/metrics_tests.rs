//! Tests for the Phase 2 performance metrics engine and edge score.

use chrono::NaiveDate;
use rust_decimal::Decimal;

mod common;
use common::{annotation_with_risk, closed_trade_from_lot, d, matched_lot, money, now};
use tantra_ledger_performance::metrics::{
    EDGE_WEIGHT_AVG_WIN_LOSS, EDGE_WEIGHT_CONSISTENCY, EDGE_WEIGHT_DRAWDOWN,
    EDGE_WEIGHT_PROFIT_FACTOR, EDGE_WEIGHT_RECOVERY, EDGE_WEIGHT_WIN_RATE, MetricsInput,
    compute_edge_score, compute_metrics,
};
use tantra_ledger_performance::model::ClosedTrade;

fn d1() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, 10).unwrap()
}
fn d2() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, 11).unwrap()
}
fn d3() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, 12).unwrap()
}

fn win_trade(lot_id: &str, day: NaiveDate) -> ClosedTrade {
    // entry 100 -> exit 110, qty 10: realized +100
    closed_trade_from_lot(&matched_lot(lot_id, "100.00", "110.00", 10, 10, false), day)
}

fn loss_trade(lot_id: &str, day: NaiveDate) -> ClosedTrade {
    // entry 100 -> exit 90, qty 10: realized -100
    closed_trade_from_lot(&matched_lot(lot_id, "100.00", "90.00", 10, 10, false), day)
}

#[test]
fn empty_trades_yield_zeros() {
    let m = compute_metrics(&MetricsInput::new(vec![], now())).unwrap();
    assert_eq!(m.total_trades, 0);
    assert_eq!(m.closed_trades, 0);
    assert_eq!(m.wins, 0);
    assert_eq!(m.losses, 0);
    assert_eq!(m.breakevens, 0);
    assert_eq!(m.net_pnl, money("0.00"));
    assert_eq!(m.gross_pnl, money("0.00"));
    assert_eq!(m.fees, money("0.00"));
    assert_eq!(m.win_rate, None);
    assert_eq!(m.day_win_rate, None);
    assert_eq!(m.trading_days, 0);
    assert_eq!(m.profit_factor, None);
    assert!(!m.profit_factor_is_infinite);
    assert_eq!(m.avg_win, None);
    assert_eq!(m.avg_loss, None);
    assert_eq!(m.expectancy, None);
    assert_eq!(m.largest_win, money("0.00"));
    assert_eq!(m.largest_loss, money("0.00"));
    assert_eq!(m.max_win_streak, 0);
    assert_eq!(m.max_loss_streak, 0);
    assert_eq!(m.current_streak, 0);
    assert_eq!(m.total_volume, money("0.00"));
    assert_eq!(m.max_drawdown, money("0.00"));
    assert_eq!(m.max_drawdown_pct, None);
    assert_eq!(m.trades_with_risk, 0);
}

#[test]
fn single_winning_trade() {
    let trades = vec![win_trade("T1", d1())];
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    assert_eq!(m.total_trades, 1);
    assert_eq!(m.wins, 1);
    assert_eq!(m.losses, 0);
    assert_eq!(m.breakevens, 0);
    assert_eq!(m.net_pnl, money("100.00"));
    assert_eq!(m.gross_pnl, money("100.00"));
    assert_eq!(m.win_rate, Some(d("1.00")));
    assert_eq!(m.trading_days, 1);
    assert_eq!(m.day_win_rate, Some(d("1.00")));
    assert_eq!(m.profit_factor, None); // no losses → infinite
    assert!(m.profit_factor_is_infinite);
    assert_eq!(m.avg_win, Some(money("100.00")));
    assert_eq!(m.avg_loss, None);
    assert_eq!(m.expectancy, Some(money("100.00")));
    assert_eq!(m.largest_win, money("100.00"));
    assert_eq!(m.largest_loss, money("0.00"));
    assert_eq!(m.max_win_streak, 1);
    assert_eq!(m.current_streak, 1);
    assert_eq!(m.trades_with_risk, 0);
}

#[test]
fn single_losing_trade() {
    let trades = vec![loss_trade("T1", d1())];
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    assert_eq!(m.losses, 1);
    assert_eq!(m.net_pnl, money("-100.00"));
    assert_eq!(m.gross_pnl, money("0.00"));
    assert_eq!(m.win_rate, Some(d("0.00")));
    assert_eq!(m.profit_factor, Some(d("0.00"))); // no profit, some loss → zero
    assert!(!m.profit_factor_is_infinite);
    assert_eq!(m.avg_win, None);
    assert_eq!(m.avg_loss, Some(money("100.00")));
    assert_eq!(m.current_streak, -1);
}

#[test]
fn mixed_trades_compute_all_metrics() {
    let trades = vec![
        closed_trade_from_lot(&matched_lot("A", "100.00", "110.00", 10, 10, false), d1()), // +100
        closed_trade_from_lot(&matched_lot("B", "100.00", "120.00", 10, 10, false), d1()), // +200
        closed_trade_from_lot(&matched_lot("C", "100.00", "95.00", 10, 10, false), d2()),  // -50
        closed_trade_from_lot(&matched_lot("D", "100.00", "85.00", 10, 10, false), d3()),  // -150
        closed_trade_from_lot(&matched_lot("E", "100.00", "100.00", 10, 10, false), d3()), // 0
    ];
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    assert_eq!(m.total_trades, 5);
    assert_eq!(m.wins, 2);
    assert_eq!(m.losses, 2);
    assert_eq!(m.breakevens, 1);
    // net = 100 + 200 - 50 - 150 + 0 = 100
    assert_eq!(m.net_pnl, money("100.00"));
    // gross_pnl = 100 + 200 = 300
    assert_eq!(m.gross_pnl, money("300.00"));
    assert_eq!(m.win_rate, Some(d("0.40")));
    assert_eq!(m.trading_days, 3);
    assert_eq!(m.day_win_rate, Some(d("0.333333333333333333333333333333"))); // 1 profitable day of 3
    // profit_factor = 300 / 200 = 1.5
    assert_eq!(m.profit_factor, Some(d("1.50")));
    assert!(!m.profit_factor_is_infinite);
    assert_eq!(m.avg_win, Some(money("150.00")));
    assert_eq!(m.avg_loss, Some(money("100.00")));
    assert_eq!(m.avg_win_loss_ratio, Some(d("1.50")));
    assert_eq!(m.expectancy, Some(money("20.00"))); // 100/5
    assert_eq!(m.largest_win, money("200.00"));
    assert_eq!(m.largest_loss, money("150.00"));
    assert_eq!(m.max_win_streak, 2);
    assert_eq!(m.max_loss_streak, 2); // T3 and T4 are consecutive losses
    assert_eq!(m.current_streak, 0); // last was breakeven
    assert_eq!(m.trades_with_risk, 0);
}

#[test]
fn profit_factor_infinite_when_no_losses() {
    let trades = vec![win_trade("A", d1()), win_trade("B", d2())];
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    assert_eq!(m.profit_factor, None);
    assert!(m.profit_factor_is_infinite);
}

#[test]
fn fees_reduce_net_pnl() {
    let mut t = win_trade("A", d1()); // +100
    t.fees = money("10.00");
    let m = compute_metrics(&MetricsInput::new(vec![t], now())).unwrap();
    assert_eq!(m.net_pnl, money("90.00"));
    assert_eq!(m.gross_pnl, money("90.00")); // net PnL of the winning trade (after fees)
    assert_eq!(m.fees, money("10.00"));
}

#[test]
fn r_multiple_and_avg_realized_r() {
    let mut t1 = win_trade("A", d1()); // net +100
    t1.annotation = annotation_with_risk("100.00"); // R = 1
    let mut t2 = win_trade("B", d2()); // net +200
    t2.annotation = annotation_with_risk("100.00"); // R = 2
    let m = compute_metrics(&MetricsInput::new(vec![t1, t2], now())).unwrap();
    assert_eq!(m.trades_with_risk, 2);
    assert_eq!(m.avg_realized_r, Some(d("1"))); // (1 + 1) / 2
}

#[test]
fn drawdown_from_equity_curve() {
    let trades = vec![win_trade("A", d1())];
    // Equity: 1000 -> 1200 (peak) -> 900 -> 1100. Max DD = 1200-900 = 300, pct = 300/1200 = 0.25
    let curve = vec![
        (
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            money("1000.00"),
        ),
        (
            NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
            money("1200.00"),
        ),
        (
            NaiveDate::from_ymd_opt(2026, 3, 3).unwrap(),
            money("900.00"),
        ),
        (
            NaiveDate::from_ymd_opt(2026, 3, 4).unwrap(),
            money("1100.00"),
        ),
    ];
    let m = compute_metrics(&MetricsInput::new(trades, now()).with_equity_curve(curve)).unwrap();
    assert_eq!(m.max_drawdown, money("300.00"));
    assert_eq!(m.max_drawdown_pct, Some(d("0.25")));
}

#[test]
fn edge_score_null_below_min_trades() {
    let trades = vec![win_trade("A", d1()), win_trade("B", d2())]; // 2 trades
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    let score = compute_edge_score(&m);
    assert_eq!(score.version, 2);
    assert_eq!(score.score, None);
    assert_eq!(score.closed_trades, 2);
}

#[test]
fn edge_score_some_at_min_trades() {
    let trades = vec![
        win_trade("A", d1()),
        win_trade("B", d2()),
        win_trade("C", d3()),
        loss_trade("D", d1()),
        loss_trade("E", d2()),
    ]; // 5 trades
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    let score = compute_edge_score(&m);
    assert_eq!(score.closed_trades, 5);
    assert!(score.score.is_some());
    let s = score.score.unwrap();
    assert!(s >= Decimal::ZERO && s <= Decimal::from(100));
}

#[test]
fn current_streak_tracks_direction() {
    // win, win, loss → current streak = -1 (one loss)
    let trades = vec![
        win_trade("A", d1()),
        win_trade("B", d2()),
        loss_trade("C", d3()),
    ];
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    assert_eq!(m.current_streak, -1);
    assert_eq!(m.max_win_streak, 2);
    assert_eq!(m.max_loss_streak, 1);

    // loss, loss → current = -2
    let trades = vec![loss_trade("A", d1()), loss_trade("B", d2())];
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    assert_eq!(m.current_streak, -2);

    // all wins → current = +n
    let trades = vec![win_trade("A", d1()), win_trade("B", d2())];
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    assert_eq!(m.current_streak, 2);
}

#[test]
fn open_trades_supplied_by_caller() {
    let trades = vec![win_trade("A", d1())];
    let m = compute_metrics(&MetricsInput::new(trades, now()).with_open_trades(3)).unwrap();
    assert_eq!(m.open_trades, 3);
}

// ---------------------------------------------------------------------------
// v2 edge score (plan 0004 / gap 0006)
// ---------------------------------------------------------------------------

#[test]
fn edge_score_v2_weights_sum_to_one() {
    // The documented blend spec: weights sum to exactly 1.
    assert_eq!(
        EDGE_WEIGHT_WIN_RATE
            + EDGE_WEIGHT_PROFIT_FACTOR
            + EDGE_WEIGHT_AVG_WIN_LOSS
            + EDGE_WEIGHT_DRAWDOWN
            + EDGE_WEIGHT_RECOVERY
            + EDGE_WEIGHT_CONSISTENCY,
        Decimal::ONE
    );
}

#[test]
fn edge_score_v2_golden_blend() {
    // Six trades: 4 wins of +100, 2 losses of -100 (net +200). Chronological
    // order is win, win, win, loss, win, loss.
    //
    // Expected v2 components (brackets documented in `compute_edge_score`):
    //   win_rate     = 4/6 → 66.666667
    //   profit_factor= 400/200 = 2 → 100
    //   avg_win_loss = 100/100 = 1 → 50
    //   drawdown     = peak 2000 → trough 1900 → 5% → 75
    //   recovery     = net 200 / dd 100 = 2 → 100
    //   consistency  = 0.6·(1 profitable day of 3) + 0.4·(streak 3/(3+1))
    //                = 0.6·33.333333 + 0.4·75 = 20 + 30 = 50
    // Composite = 0.20·66.666667 + 0.25·100 + 0.15·50 + 0.20·75
    //           + 0.10·100 + 0.10·50 = 75.833333
    let trades = vec![
        win_trade("A", d1()),
        win_trade("B", d1()),
        win_trade("C", d2()),
        loss_trade("D", d2()),
        win_trade("E", d3()),
        loss_trade("F", d3()),
    ];
    let curve = vec![
        (
            NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
            money("1000.00"),
        ),
        (
            NaiveDate::from_ymd_opt(2026, 2, 2).unwrap(),
            money("2000.00"),
        ),
        (
            NaiveDate::from_ymd_opt(2026, 2, 3).unwrap(),
            money("1900.00"),
        ),
    ];
    let m = compute_metrics(&MetricsInput::new(trades, now()).with_equity_curve(curve)).unwrap();
    let score = compute_edge_score(&m);

    assert_eq!(score.version, 2);
    assert_eq!(score.closed_trades, 6);
    assert_eq!(score.components.win_rate.round_dp(6), d("66.666667"));
    assert_eq!(score.components.profit_factor, d("100"));
    assert_eq!(score.components.avg_win_loss, d("50"));
    assert_eq!(score.components.drawdown, d("75"));
    assert_eq!(score.components.recovery, d("100"));
    assert_eq!(score.components.consistency.round_dp(6), d("50.000000"));
    assert_eq!(score.score.unwrap().round_dp(6), d("75.833333"));
}

#[test]
fn edge_score_v2_no_losses_pins_profit_factor_component() {
    // Five all-winning trades: profit factor is infinite → component 100,
    // and recovery/consistency are well-defined.
    let trades = vec![
        win_trade("A", d1()),
        win_trade("B", d1()),
        win_trade("C", d2()),
        win_trade("D", d3()),
        win_trade("E", d3()),
    ];
    let m = compute_metrics(&MetricsInput::new(trades, now())).unwrap();
    let score = compute_edge_score(&m);
    assert_eq!(score.components.profit_factor, d("100"));
    assert!(score.score.unwrap() >= d("50"));
    assert!(score.score.unwrap() <= d("100"));
}
