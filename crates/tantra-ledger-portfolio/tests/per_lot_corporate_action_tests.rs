//! Per-lot corporate-action cost-basis tests (gap 0008, plan 0005).
//!
//! The aggregate corporate-action helpers only adjust the aggregate
//! `HeldPosition`; FIFO cost-basis integrity lives in the `TaxLot` book, so
//! split/bonus/demerger adjustments must be mirrored onto every lot's
//! quantity, entry price and remaining quantity. These tests pin the
//! per-lot behavior with exact `Decimal` expectations and prove that
//! `AccountState::match_lots` realizes the correct per-lot PnL after an
//! adjustment (the reason the aggregate-level approximation was wrong).

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use tantra_core::{
    AccountId, Currency, Exchange, InstrumentId, OrderSide, PositionSide, Price, Quantity,
};
use tantra_ledger_portfolio::{
    AccountState, CorporateAction, LotFill, TaxLot, adjust_lot_for_bonus, adjust_lot_for_demerger,
    adjust_lot_for_split, reduce_lot_for_demerger,
};

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 9, 15, 0).unwrap()
}

fn reliance() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "RELIANCE").unwrap()
}

fn acme() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "ACME").unwrap()
}

fn lot(id: &str, qty: u64, price: &str) -> TaxLot {
    TaxLot {
        lot_id: id.to_string(),
        account_id: AccountId::new("acct1"),
        instrument: reliance(),
        side: PositionSide::Long,
        entry_price: Price::from_rupees(price).unwrap(),
        quantity: Quantity::from_u64(qty),
        remaining_quantity: Quantity::from_u64(qty),
        entry_date: NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
        created_at: fixed_now(),
    }
}

// ————————————————————————————————————————
// Per-lot split
// ————————————————————————————————————————

#[test]
fn test_split_adjusts_each_lot_quantity_and_entry_price() {
    let mut l1 = lot("L1", 100, "1000");
    let mut l2 = lot("L2", 50, "800");

    // 2:1 split: quantity doubles, price halves, cost basis preserved.
    adjust_lot_for_split(&mut l1, 2, 1);
    adjust_lot_for_split(&mut l2, 2, 1);

    assert_eq!(l1.quantity, Quantity::from_u64(200));
    assert_eq!(l1.remaining_quantity, Quantity::from_u64(200));
    assert_eq!(l1.entry_price, Price::from_rupees("500").unwrap());

    assert_eq!(l2.quantity, Quantity::from_u64(100));
    assert_eq!(l2.remaining_quantity, Quantity::from_u64(100));
    assert_eq!(l2.entry_price, Price::from_rupees("400").unwrap());
}

#[test]
fn test_split_ratio_is_fractional() {
    let mut l = lot("L1", 300, "1200");
    // 2:3 split: 300 * 2/3 = 200 shares, price 1200 / (2/3) = 1800.
    // `Decimal` division keeps 28 digits of scale, so 2/3 is inexact: compare
    // with a paise-level tolerance.
    adjust_lot_for_split(&mut l, 2, 3);
    let tolerance = Decimal::new(1, 8); // 0.00000001
    assert!((l.quantity.value() - Decimal::from(200)).abs() < tolerance);
    assert!((l.remaining_quantity.value() - Decimal::from(200)).abs() < tolerance);
    assert!((l.entry_price.value() - Decimal::from(1800)).abs() < tolerance);
}

// ————————————————————————————————————————
// Per-lot bonus
// ————————————————————————————————————————

#[test]
fn test_bonus_scales_quantity_and_rebases_entry_price() {
    let mut l = lot("L1", 100, "1000");
    // 1:1 bonus: 100 → 200 shares, price 1000 → 500 (total basis unchanged).
    adjust_lot_for_bonus(&mut l, 1, 1);
    assert_eq!(l.quantity, Quantity::from_u64(200));
    assert_eq!(l.remaining_quantity, Quantity::from_u64(200));
    assert_eq!(l.entry_price, Price::from_rupees("500").unwrap());
}

#[test]
fn test_bonus_fractional_ratio() {
    let mut l = lot("L1", 200, "1200");
    // 1:4 bonus: 200 * (1 + 1/4) = 250, price 1200 / 1.25 = 960.
    adjust_lot_for_bonus(&mut l, 1, 4);
    assert_eq!(l.quantity, Quantity::try_new(Decimal::from(250)).unwrap());
    assert_eq!(
        l.remaining_quantity,
        Quantity::try_new(Decimal::from(250)).unwrap()
    );
    assert_eq!(l.entry_price, Price::from_rupees("960").unwrap());
}

// ————————————————————————————————————————
// Per-lot demerger
// ————————————————————————————————————————

#[test]
fn test_demerger_splits_each_lot_basis_and_creates_new_lot() {
    let mut old = lot("L1", 100, "1000");

    // 1:1 allocation, 40% of the original basis moves to the new scrip.
    let new = adjust_lot_for_demerger(
        &old,
        acme(),
        1,
        1,
        Decimal::new(40, 2), // 0.40
    )
    .expect("new lot created");

    assert_eq!(new.instrument, acme());
    assert_eq!(new.lot_id, "DEMERGER_L1");
    assert_eq!(new.quantity, Quantity::from_u64(100));
    assert_eq!(new.remaining_quantity, Quantity::from_u64(100));
    // 1000 * 0.40 / 1 = 400
    assert_eq!(new.entry_price, Price::from_rupees("400").unwrap());

    // Original lot keeps 60% of its basis; quantity unchanged.
    reduce_lot_for_demerger(&mut old, Decimal::new(40, 2));
    assert_eq!(old.quantity, Quantity::from_u64(100));
    assert_eq!(old.entry_price, Price::from_rupees("600").unwrap());
}

#[test]
fn test_demerger_zero_allocation_yields_no_new_lot() {
    let old = lot("L1", 100, "1000");
    let none = adjust_lot_for_demerger(&old, acme(), 0, 1, Decimal::new(40, 2));
    assert!(none.is_none());
}

// ————————————————————————————————————————
// Account-level: per-lot adjustments + FIFO integrity
// ————————————————————————————————————————

#[test]
fn test_account_per_lot_split_then_fifo_match_uses_adjusted_basis() {
    let mut acc = AccountState::new(Currency::Inr, vec![]);
    acc.lots.push(lot("L1", 100, "1000"));
    acc.lots.push(lot("L2", 50, "800"));

    let split = CorporateAction::Split {
        ratio_numerator: 2,
        ratio_denominator: 1,
        ex_date: NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
    };
    let income =
        acc.apply_corporate_actions_to_lots(&[split], NaiveDate::from_ymd_opt(2026, 4, 1).unwrap());
    assert_eq!(income.amount, Decimal::ZERO);

    // Lots adjusted: L1 = 200 @ 500, L2 = 100 @ 400.
    assert_eq!(acc.lots[0].quantity, Quantity::from_u64(200));
    assert_eq!(acc.lots[0].entry_price, Price::from_rupees("500").unwrap());
    assert_eq!(acc.lots[1].quantity, Quantity::from_u64(100));
    assert_eq!(acc.lots[1].entry_price, Price::from_rupees("400").unwrap());

    // Position rebuilt as a projection over the lot book: 300 @ weighted avg.
    assert_eq!(acc.positions.len(), 1);
    assert_eq!(acc.positions[0].quantity, Quantity::from_u64(300));
    // (200*500 + 100*400) / 300 = 140000/300 = 466.666...
    assert_eq!(
        acc.positions[0].entry_price.value(),
        Decimal::from(140000) / Decimal::from(300)
    );

    // Sell 150 @ 600: FIFO matches L1 (200 remaining) entirely: pnl =
    // 150 * (600 - 500) = 15,000. L1 remaining drops to 50.
    let fill = LotFill {
        instrument: reliance(),
        side: OrderSide::Sell,
        price: Price::from_rupees("600").unwrap(),
        quantity: Quantity::from_u64(150),
        timestamp: fixed_now(),
    };
    let matched = acc.match_lots(&fill);
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].lot_id, "L1");
    assert_eq!(matched[0].matched_quantity, Quantity::from_u64(150));
    assert_eq!(matched[0].realized_pnl.amount, Decimal::from(15_000));
    assert_eq!(acc.lots[0].remaining_quantity, Quantity::from_u64(50));
    assert_eq!(acc.lots[1].remaining_quantity, Quantity::from_u64(100));
}

#[test]
fn test_account_per_lot_dividend_accrues_from_remaining_quantity() {
    let mut acc = AccountState::new(Currency::Inr, vec![]);
    acc.lots.push(lot("L1", 100, "1000"));
    acc.lots.push(lot("L2", 50, "800"));

    let dividend = CorporateAction::Dividend {
        amount_per_share: 500, // 500 paise = ₹5 per share
        ex_date: NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        record_date: NaiveDate::from_ymd_opt(2026, 3, 5).unwrap(),
        payment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
    };
    let income = acc
        .apply_corporate_actions_to_lots(&[dividend], NaiveDate::from_ymd_opt(2026, 4, 1).unwrap());
    // (100 + 50) * 500 paise = 75,000 paise.
    assert_eq!(income.amount, Decimal::from(75_000));
    // Dividend does not touch lot quantities or basis.
    assert_eq!(acc.lots[0].quantity, Quantity::from_u64(100));
    assert_eq!(acc.lots[0].entry_price, Price::from_rupees("1000").unwrap());
}

#[test]
fn test_account_per_lot_demerger_creates_new_position_for_new_instrument() {
    let mut acc = AccountState::new(Currency::Inr, vec![]);
    acc.lots.push(lot("L1", 100, "1000"));

    let demerger = CorporateAction::Demerger {
        new_instrument: acme(),
        allocation_numerator: 1,
        allocation_denominator: 1,
        cost_basis_allocation_pct: Decimal::new(40, 2),
        ex_date: NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
    };
    let income = acc
        .apply_corporate_actions_to_lots(&[demerger], NaiveDate::from_ymd_opt(2026, 4, 1).unwrap());
    assert_eq!(income.amount, Decimal::ZERO);

    // Original lot keeps 60% basis; a DEMERGER lot for the new scrip exists.
    assert_eq!(acc.lots.len(), 2);
    assert_eq!(acc.lots[0].entry_price, Price::from_rupees("600").unwrap());
    let new_lot = acc
        .lots
        .iter()
        .find(|l| l.instrument == acme())
        .expect("new lot");
    assert_eq!(new_lot.entry_price, Price::from_rupees("400").unwrap());
    assert_eq!(new_lot.quantity, Quantity::from_u64(100));

    // Both scrips appear as positions (projection over the lot book).
    assert_eq!(acc.positions.len(), 2);
}

#[test]
fn test_account_lot_adjustment_skips_future_ex_dates() {
    let mut acc = AccountState::new(Currency::Inr, vec![]);
    acc.lots.push(lot("L1", 100, "1000"));

    let split = CorporateAction::Split {
        ratio_numerator: 2,
        ratio_denominator: 1,
        ex_date: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
    };
    // as_of before ex-date: not applied.
    acc.apply_corporate_actions_to_lots(&[split], NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());
    assert_eq!(acc.lots[0].quantity, Quantity::from_u64(100));
    assert_eq!(acc.lots[0].entry_price, Price::from_rupees("1000").unwrap());
}
