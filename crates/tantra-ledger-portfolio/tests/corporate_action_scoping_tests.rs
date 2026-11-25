//! Corporate-action instrument scoping, ex-date eligibility, idempotency,
//! rupee-denominated dividends, and collision-free demerger lot IDs
//! (gap 0014, plan 0006).

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use proptest::prelude::*;
use rust_decimal::Decimal;
use tantra_core::{
    AccountId, Currency, Exchange, InstrumentId, Money, PositionSide, Price, Quantity,
};
use tantra_ledger_portfolio::{
    AccountState, ActionId, CORPORATE_ACTION_WIRE_VERSION, CorporateAction, HeldPosition, TaxLot,
    apply_corporate_actions_to_account,
};

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 9, 15, 0).unwrap()
}

fn reliance() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "RELIANCE").unwrap()
}

fn tcs() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "TCS").unwrap()
}

fn jiofin() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "JIOFIN").unwrap()
}

fn beta() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "BETA").unwrap()
}

fn make_lot(
    id: &str,
    instrument: InstrumentId,
    qty: u64,
    price: &str,
    entry_date: NaiveDate,
) -> TaxLot {
    TaxLot {
        lot_id: id.to_string(),
        account_id: AccountId::new("acct1"),
        instrument,
        side: PositionSide::Long,
        entry_price: Price::from_rupees(price).unwrap(),
        quantity: Quantity::from_u64(qty),
        remaining_quantity: Quantity::from_u64(qty),
        entry_date,
        created_at: fixed_now(),
    }
}

proptest! {
    #[test]
    fn prop_corporate_action_on_target_never_mutates_other_instrument(
        rel_qty in 10u64..1000,
        tcs_qty in 10u64..1000,
        rel_price in 100u64..5000,
        tcs_price in 500u64..6000,
        num in 1u64..6,
        den in 1u64..4,
        is_bonus in any::<bool>(),
    ) {
        let entry_date = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
        let ex_date = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        let as_of = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();

        let mut acc = AccountState::new(Currency::Inr, vec![]);
        acc.record_lot(&make_lot("REL1", reliance(), rel_qty, &rel_price.to_string(), entry_date));
        acc.record_lot(&make_lot("TCS1", tcs(), tcs_qty, &tcs_price.to_string(), entry_date));

        let tcs_lot_before = acc.lots[1].clone();
        let tcs_pos_before_qty = acc.positions[1].quantity;
        let tcs_pos_before_price = acc.positions[1].entry_price;

        let action = if is_bonus {
            CorporateAction::Bonus {
                instrument: reliance(),
                ratio_numerator: num,
                ratio_denominator: den,
                ex_date,
            }
        } else {
            CorporateAction::Split {
                instrument: reliance(),
                ratio_numerator: num,
                ratio_denominator: den,
                ex_date,
            }
        };

        let dividend = acc.apply_corporate_actions_to_lots(&[action], as_of);
        prop_assert_eq!(dividend, Money::inr(Decimal::ZERO));

        // TCS lot and position must be completely untouched.
        prop_assert_eq!(&acc.lots[1], &tcs_lot_before);
        let tcs_pos_after = acc.positions.iter().find(|p| p.instrument == tcs()).unwrap();
        prop_assert_eq!(tcs_pos_after.quantity, tcs_pos_before_qty);
        prop_assert_eq!(tcs_pos_after.entry_price, tcs_pos_before_price);

        // RELIANCE lot must be scaled and preserve total cost basis (within rounding tolerance).
        let expected_factor = if is_bonus {
            Decimal::ONE + Decimal::from(num) / Decimal::from(den)
        } else {
            Decimal::from(num) / Decimal::from(den)
        };
        let rel_lot_after = &acc.lots[0];
        let old_basis = Decimal::from(rel_qty) * Decimal::from(rel_price);
        let new_basis = rel_lot_after.quantity.value() * rel_lot_after.entry_price.value();
        let tol = Decimal::new(1, 6);
        prop_assert!((rel_lot_after.quantity.value() - Decimal::from(rel_qty) * expected_factor).abs() < tol);
        prop_assert!((new_basis - old_basis).abs() < tol);
    }
}

#[test]
fn test_idempotent_double_apply_across_replays_and_ticks() {
    let entry_date = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
    let ex_date = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
    let tick1 = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
    let tick2 = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();

    let mut acc = AccountState::new(Currency::Inr, vec![]);
    acc.record_lot(&make_lot("L1", reliance(), 100, "1000", entry_date));

    let actions = vec![
        CorporateAction::Split {
            instrument: reliance(),
            ratio_numerator: 2,
            ratio_denominator: 1,
            ex_date,
        },
        CorporateAction::Dividend {
            instrument: reliance(),
            amount_per_share: Money::inr(Decimal::from(5)),
            ex_date: NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
            record_date: NaiveDate::from_ymd_opt(2026, 3, 18).unwrap(),
            payment_date: NaiveDate::from_ymd_opt(2026, 3, 25).unwrap(),
        },
    ];

    // On tick1 (2026-03-01): only Split applies; Dividend ex_date (03-15) is still in future.
    let div1 = acc.apply_corporate_actions_to_lots(&actions, tick1);
    assert_eq!(div1, Money::inr(Decimal::ZERO));
    assert_eq!(acc.lots[0].quantity, Quantity::from_u64(200));
    assert_eq!(acc.lots[0].entry_price, Price::from_rupees("500").unwrap());
    assert_eq!(acc.applied_corporate_actions.len(), 1);

    // On tick2 (2026-04-01): Split must NOT re-apply; Dividend applies once against 200 shares.
    let div2 = acc.apply_corporate_actions_to_lots(&actions, tick2);
    assert_eq!(div2, Money::inr(Decimal::from(1000)));
    assert_eq!(acc.lots[0].quantity, Quantity::from_u64(200));
    assert_eq!(acc.lots[0].entry_price, Price::from_rupees("500").unwrap());
    assert_eq!(acc.applied_corporate_actions.len(), 2);

    // Replaying the full feed a third time is a complete no-op.
    let div3 = acc.apply_corporate_actions_to_lots(&actions, tick2);
    assert_eq!(div3, Money::inr(Decimal::ZERO));
    assert_eq!(acc.lots[0].quantity, Quantity::from_u64(200));
    assert_eq!(acc.lots[0].entry_price, Price::from_rupees("500").unwrap());
    assert_eq!(acc.applied_corporate_actions.len(), 2);
}

#[test]
fn test_idempotent_account_helper_filters_instrument_and_ex_date() {
    let ex_date = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
    let as_of = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();

    let mut acc = AccountState::new(
        Currency::Inr,
        vec![
            HeldPosition::new(
                reliance(),
                PositionSide::Long,
                Quantity::from_u64(10),
                Price::from_rupees("2000").unwrap(),
            )
            .with_entry_date(NaiveDate::from_ymd_opt(2026, 2, 1).unwrap()),
            HeldPosition::new(
                tcs(),
                PositionSide::Long,
                Quantity::from_u64(20),
                Price::from_rupees("3500").unwrap(),
            )
            .with_entry_date(NaiveDate::from_ymd_opt(2026, 2, 1).unwrap()),
        ],
    );

    let split = CorporateAction::Split {
        instrument: reliance(),
        ratio_numerator: 2,
        ratio_denominator: 1,
        ex_date,
    };

    let _ = apply_corporate_actions_to_account(&mut acc, std::slice::from_ref(&split), as_of);
    assert_eq!(acc.positions[0].quantity, Quantity::from_u64(20));
    assert_eq!(
        acc.positions[0].entry_price,
        Price::from_rupees("1000").unwrap()
    );
    assert_eq!(acc.positions[1].quantity, Quantity::from_u64(20));
    assert_eq!(
        acc.positions[1].entry_price,
        Price::from_rupees("3500").unwrap()
    );

    // Double apply is a no-op.
    let _ = apply_corporate_actions_to_account(&mut acc, &[split], as_of);
    assert_eq!(acc.positions[0].quantity, Quantity::from_u64(20));
    assert_eq!(
        acc.positions[0].entry_price,
        Price::from_rupees("1000").unwrap()
    );
}

#[test]
fn test_post_and_on_ex_date_lots_remain_untouched() {
    let pre_ex = NaiveDate::from_ymd_opt(2026, 2, 28).unwrap();
    let ex_date = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
    let post_ex = NaiveDate::from_ymd_opt(2026, 3, 2).unwrap();
    let as_of = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();

    let mut acc = AccountState::new(Currency::Inr, vec![]);
    acc.record_lot(&make_lot("L_PRE", reliance(), 100, "2000", pre_ex));
    acc.record_lot(&make_lot("L_ON", reliance(), 50, "1000", ex_date));
    acc.record_lot(&make_lot("L_POST", reliance(), 25, "1050", post_ex));

    let split = CorporateAction::Split {
        instrument: reliance(),
        ratio_numerator: 2,
        ratio_denominator: 1,
        ex_date,
    };

    acc.apply_corporate_actions_to_lots(&[split], as_of);

    // Pre-ex lot scales 100 @ 2000 -> 200 @ 1000.
    assert_eq!(acc.lots[0].quantity, Quantity::from_u64(200));
    assert_eq!(acc.lots[0].entry_price, Price::from_rupees("1000").unwrap());

    // On-ex and post-ex lots already traded ex-split and remain untouched.
    assert_eq!(acc.lots[1].quantity, Quantity::from_u64(50));
    assert_eq!(acc.lots[1].entry_price, Price::from_rupees("1000").unwrap());
    assert_eq!(acc.lots[2].quantity, Quantity::from_u64(25));
    assert_eq!(acc.lots[2].entry_price, Price::from_rupees("1050").unwrap());
}

#[test]
fn test_golden_dividend_in_rupees_and_currency_isolation() {
    let pre_ex = NaiveDate::from_ymd_opt(2026, 2, 20).unwrap();
    let ex_date = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
    let as_of = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();

    let mut acc = AccountState::new(Currency::Inr, vec![]);
    acc.record_lot(&make_lot("L1", reliance(), 100, "1000", pre_ex));
    acc.record_lot(&make_lot("L2", reliance(), 50, "1100", pre_ex));
    acc.record_lot(&make_lot("L3_EX", reliance(), 25, "1050", ex_date));
    acc.record_lot(&make_lot("L4_TCS", tcs(), 40, "3500", pre_ex));

    // Exact decimal ₹5.25 per share -> 150 eligible shares * 5.25 = ₹787.50.
    let dividend_inr = CorporateAction::Dividend {
        instrument: reliance(),
        amount_per_share: Money::inr(Decimal::new(525, 2)),
        ex_date,
        record_date: NaiveDate::from_ymd_opt(2026, 3, 3).unwrap(),
        payment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
    };
    // Mismatched currency (USD on INR account) must be skipped and not marked applied.
    let dividend_usd = CorporateAction::Dividend {
        instrument: reliance(),
        amount_per_share: Money::new(Decimal::from(5), Currency::Usd),
        ex_date: NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
        record_date: NaiveDate::from_ymd_opt(2026, 3, 4).unwrap(),
        payment_date: NaiveDate::from_ymd_opt(2026, 3, 12).unwrap(),
    };

    let income = acc.apply_corporate_actions_to_lots(&[dividend_inr, dividend_usd.clone()], as_of);
    assert_eq!(income, Money::inr(Decimal::new(78750, 2)));
    assert!(
        !acc.applied_corporate_actions
            .contains(&dividend_usd.action_id())
    );
}

#[test]
fn test_double_demerger_produces_distinct_lot_ids() {
    let pre_ex = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
    let d1_ex = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
    let d2_ex = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
    let as_of = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();

    let mut acc = AccountState::new(Currency::Inr, vec![]);
    acc.record_lot(&make_lot("L1", reliance(), 100, "1000", pre_ex));

    let dm1 = CorporateAction::Demerger {
        instrument: reliance(),
        new_instrument: jiofin(),
        allocation_numerator: 1,
        allocation_denominator: 1,
        cost_basis_allocation_pct: Decimal::new(20, 2),
        ex_date: d1_ex,
    };
    let dm2 = CorporateAction::Demerger {
        instrument: reliance(),
        new_instrument: beta(),
        allocation_numerator: 1,
        allocation_denominator: 2,
        cost_basis_allocation_pct: Decimal::new(25, 2),
        ex_date: d2_ex,
    };

    let id1 = ActionId::from_action(&dm1);
    let id2 = ActionId::from_action(&dm2);
    assert_ne!(id1, id2);

    acc.apply_corporate_actions_to_lots(&[dm1, dm2], as_of);

    assert_eq!(acc.lots.len(), 3);
    let dm1_lot = acc
        .lots
        .iter()
        .find(|l| l.instrument == jiofin())
        .expect("jiofin demerger lot");
    let dm2_lot = acc
        .lots
        .iter()
        .find(|l| l.instrument == beta())
        .expect("beta demerger lot");

    assert_eq!(dm1_lot.lot_id, format!("L1:DM:{id1}"));
    assert_eq!(dm2_lot.lot_id, format!("L1:DM:{id2}"));
    assert_ne!(dm1_lot.lot_id, dm2_lot.lot_id);

    // L1: 1000 * 0.80 = 800 after dm1, then 800 * 0.75 = 600 after dm2.
    assert_eq!(acc.lots[0].entry_price, Price::from_rupees("600").unwrap());
    // dm1_lot: 100 shares @ 1000 * 0.20 / 1 = 200.
    assert_eq!(dm1_lot.quantity, Quantity::from_u64(100));
    assert_eq!(dm1_lot.entry_price, Price::from_rupees("200").unwrap());
    // dm2_lot: 50 shares @ 800 * 0.25 / 0.5 = 400.
    assert_eq!(dm2_lot.quantity, Quantity::from_u64(50));
    assert_eq!(dm2_lot.entry_price, Price::from_rupees("400").unwrap());
}

#[test]
fn test_wire_version_bumped_and_legacy_unscoped_payload_rejected() {
    assert_eq!(CORPORATE_ACTION_WIRE_VERSION, 2);

    let legacy_split =
        r#"{"Split":{"ratio_numerator":2,"ratio_denominator":1,"ex_date":"2026-03-01"}}"#;
    assert!(
        serde_json::from_str::<CorporateAction>(legacy_split).is_err(),
        "unscoped pre-v2 Split payload must fail deserialization"
    );

    let legacy_dividend = r#"{"Dividend":{"amount_per_share":500,"ex_date":"2026-03-01","record_date":"2026-03-05","payment_date":"2026-03-10"}}"#;
    assert!(
        serde_json::from_str::<CorporateAction>(legacy_dividend).is_err(),
        "unscoped pre-v2 i64 Dividend payload must fail deserialization"
    );
}
