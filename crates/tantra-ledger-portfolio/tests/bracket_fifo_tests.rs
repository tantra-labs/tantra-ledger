//! FIFO bracket-exit booking tests (gap 0012 follow-on, plan 0005 T12.7).
//!
//! `apply_bracket_exits` replays a bracket's exit legs (the `PartialExit` +
//! `StopTriggered` events, normalized to `(price, qty)` tuples) through the
//! FIFO position engine (`apply_fill_to_account`) so the account's realized
//! P&L is booked against its FIFO cost basis — which can differ from the
//! bracket's single-anchored `(exit − entry) × qty` audit P&L when the account
//! already holds earlier lots in the same instrument.
//!
//! Covered:
//! - (a) an account with an earlier long lot at a lower price plus a bracket
//!   entry at a higher price: the FIFO booking realises the earlier lot's
//!   price advantage (oldest-lot-first), and the total differs from the naive
//!   single-anchor by exactly `(fill − old_entry) × old_qty`.
//! - (b) an account holding exactly the bracket position (no earlier lots)
//!   behaves like a manual `apply_fill_to_account` loop and reproduces the
//!   bracket's own naive anchor.
//! - (c) exit quantities are clamped to the available position (short-flip
//!   guard): over-exits never drive the position negative or reverse it, and
//!   a truly empty account is a safe no-op.
//! - proptest: random exit sequences never drive `AccountState` into negative
//!   quantities or flip the position side.

use proptest::prelude::*;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use tantra_core::{Exchange, InstrumentId};
use tantra_ledger_portfolio::{
    AccountState, Currency, HeldPosition, Money, PositionSide, Price, Quantity,
    apply_bracket_exits, apply_fill_to_account,
};

fn reliance() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "RELIANCE").unwrap()
}

fn rupees(s: &str) -> Price {
    Price::from_rupees(s).expect("valid price")
}

fn shares(n: u64) -> Quantity {
    Quantity::from_u64(n)
}

/// Scenario (a): the account already holds 100 shares @ 900 (an earlier long
/// lot), then a bracket enters 300 shares @ 1000. The FIFO cost basis of the
/// 400-share position is the remaining-quantity-weighted average of the open
/// lots: `(100×900 + 300×1000) / 400 = 975`. The bracket's naive audit P&L is
/// anchored at its own entry (1000).
#[test]
fn test_fifo_booked_against_oldest_lot_before_bracket_anchor() {
    let mut account = AccountState::new(
        Currency::Inr,
        vec![HeldPosition::new(
            reliance(),
            PositionSide::Long,
            shares(400),
            rupees("975"), // (100×900 + 300×1000) / 400 — exact Decimal
        )],
    );

    // Exit legs in fill order: partial exit 100 @ 1100, then stop close 300 @ 1050.
    let exits = [(rupees("1100"), shares(100)), (rupees("1050"), shares(300))];
    let fifo_pnl = apply_bracket_exits(&mut account, &reliance(), PositionSide::Long, &exits);

    // FIFO against the oldest lot first (strict per-lot decomposition):
    //   exit 1 consumes the earlier lot (100 @ 900): (1100 − 900) × 100
    //   exit 2 consumes the bracket lot (300 @ 1000): (1050 − 1000) × 300
    let strict_fifo = (Decimal::from(1100) - Decimal::from(900)) * Decimal::from(100)
        + (Decimal::from(1050) - Decimal::from(1000)) * Decimal::from(300);
    assert_eq!(fifo_pnl, Money::inr(Decimal::from(35_000)));
    assert_eq!(fifo_pnl.amount, strict_fifo);
    assert_eq!(fifo_pnl.amount, Decimal::from(35_000));

    // Naive single-anchor (bracket entry 1000) audit P&L:
    //   (1100 − 1000) × 100 + (1050 − 1000) × 300 = 25,000
    let naive = (Decimal::from(1100) - Decimal::from(1000)) * Decimal::from(100)
        + (Decimal::from(1050) - Decimal::from(1000)) * Decimal::from(300);
    assert_eq!(naive, Decimal::from(25_000));

    // The FIFO total differs from the naive anchor by exactly the earlier
    // lot's price advantage over the bracket entry, scaled by its quantity:
    // (fill − old_entry) × qty = (1000 − 900) × 100 = 10,000.
    let expected_diff = (Decimal::from(1000) - Decimal::from(900)) * Decimal::from(100);
    assert_eq!(expected_diff, Decimal::from(10_000));
    assert_eq!(fifo_pnl.amount - naive, expected_diff);

    // The whole position was consumed; quantity is zero and the side never flipped.
    assert_eq!(account.positions.len(), 1);
    assert_eq!(account.positions[0].quantity, Quantity::ZERO);
    assert_eq!(account.positions[0].side, PositionSide::Long);
    assert_eq!(
        account.positions[0].realized_pnl.amount,
        Decimal::from(35_000)
    );
}

/// Scenario (b): an account holding exactly the bracket's position — no other
/// or earlier lots — behaves like a manual `apply_fill_to_account` loop, and
/// reproduces the bracket's own naive single-anchor P&L exactly (FIFO ==
/// anchor when the bracket lot is the only lot).
#[test]
fn test_bracket_only_account_matches_naive_anchor_and_manual_loop() {
    let exits = [(rupees("1100"), shares(100)), (rupees("1050"), shares(200))];

    let mut via_helper = AccountState::new(
        Currency::Inr,
        vec![HeldPosition::new(
            reliance(),
            PositionSide::Long,
            shares(300),
            rupees("1000"),
        )],
    );
    let helper_pnl = apply_bracket_exits(&mut via_helper, &reliance(), PositionSide::Long, &exits);

    // Manual loop through `apply_fill_to_account` for every exit leg.
    let mut manual = AccountState::new(
        Currency::Inr,
        vec![HeldPosition::new(
            reliance(),
            PositionSide::Long,
            shares(300),
            rupees("1000"),
        )],
    );
    let mut manual_pnl = Money::inr(Decimal::ZERO);
    for (price, qty) in exits {
        let realized = apply_fill_to_account(
            &mut manual,
            reliance(),
            PositionSide::Long,
            qty.value().to_u64().expect("whole shares"),
            price,
        );
        manual_pnl = Money::inr(manual_pnl.amount + realized.amount);
    }

    assert_eq!(helper_pnl, manual_pnl);
    assert_eq!(manual_pnl.amount, Decimal::from(20_000));
    // Naive single-anchor (bracket entry 1000): the only lot, so FIFO == naive.
    let naive = (Decimal::from(1100) - Decimal::from(1000)) * Decimal::from(100)
        + (Decimal::from(1050) - Decimal::from(1000)) * Decimal::from(200);
    assert_eq!(helper_pnl.amount, naive);
    assert_eq!(helper_pnl.amount, Decimal::from(20_000));
    assert_eq!(via_helper.positions[0].quantity, Quantity::ZERO);
}

/// Scenario (c): an exit stream whose total exceeds the available position is
/// clamped to the available quantity — the account-side short-flip guard — so
/// the FIFO position is never driven negative and never reverses.
#[test]
fn test_exit_quantity_clamped_to_available_position_never_flips() {
    let mut account = AccountState::new(
        Currency::Inr,
        vec![HeldPosition::new(
            reliance(),
            PositionSide::Long,
            shares(200),
            rupees("1000"),
        )],
    );

    // Total exits 100 + 100 + 100 = 300 > 200 available: the third leg must be
    // clamped away entirely (200-booked), realizing
    //   (1100 − 1000) × 100 + (1050 − 1000) × 100 = 15,000.
    let exits = [
        (rupees("1100"), shares(100)),
        (rupees("1050"), shares(100)),
        (rupees("1000"), shares(100)),
    ];
    let pnl = apply_bracket_exits(&mut account, &reliance(), PositionSide::Long, &exits);

    assert_eq!(pnl.amount, Decimal::from(15_000));
    assert_eq!(account.positions.len(), 1);
    assert_eq!(account.positions[0].quantity, Quantity::ZERO);
    assert_eq!(
        account.positions[0].side,
        PositionSide::Long,
        "over-exits must never flip the position short"
    );
}

/// Scenario (c): on an account with no position in the instrument, every exit
/// is clamped to zero — a safe no-op that creates no position and realises no
/// P&L (unlike a bare `apply_fill_to_account` call, which would open one).
#[test]
fn test_empty_account_is_safe_noop() {
    let mut account = AccountState::new(Currency::Inr, Vec::new());
    let exits = [(rupees("1100"), shares(100)), (rupees("1050"), shares(200))];
    let pnl = apply_bracket_exits(&mut account, &reliance(), PositionSide::Long, &exits);

    assert_eq!(pnl, Money::inr(Decimal::ZERO));
    assert!(account.positions.is_empty(), "no position may be opened");
    assert!(account.lots.is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Any random exit sequence (including over-exits and zero positions) must
    /// never drive the account's position quantity negative or flip its side:
    /// the clamp guard in `apply_bracket_exits` is the invariant.
    #[test]
    fn exits_never_drive_position_negative_or_flip_side(
        position_qty in 0u64..10_000u64,
        exits in prop::collection::vec(
            (900u64..1_200u64, 0u64..20_000u64),
            0..40,
        ),
    ) {
        let mut account = AccountState::new(
            Currency::Inr,
            if position_qty == 0 {
                Vec::new()
            } else {
                vec![HeldPosition::new(
                    reliance(),
                    PositionSide::Long,
                    shares(position_qty),
                    rupees("1000"),
                )]
            },
        );
        let exit_tuples: Vec<(Price, Quantity)> = exits
            .into_iter()
            .map(|(price, qty)| (rupees(&price.to_string()), shares(qty)))
            .collect();
        let _pnl = apply_bracket_exits(&mut account, &reliance(), PositionSide::Long, &exit_tuples);

        for pos in &account.positions {
            prop_assert!(
                pos.quantity.value() >= Decimal::ZERO,
                "position quantity went negative: {}",
                pos.quantity.value()
            );
            prop_assert_eq!(pos.side, PositionSide::Long, "position side flipped");
        }
    }
}
