//! Integration, golden-vector, and property tests for `tantra_ledger_allocation::fire` (`T19.1`).

use proptest::prelude::*;
use rust_decimal::Decimal;
use serde::Deserialize;
use tantra_core::{Currency, Money};
use tantra_ledger_allocation::{
    CashflowBucketConfig, ExpenseCategory, ExpenseInputMode, ExpenseItem, FireArchetypeId,
    FireError, FirePlanConfig, SorStressConfig, TaxDragConfig, calculate_coast_fire_corpus,
    calculate_fire_archetypes, effective_net_annual_return_pct, run_sor_stress,
    simulate_fire_runway, simulate_fire_runway_with_extras, simulate_post_fire_cashflow,
};

#[derive(Debug, Deserialize)]
struct GoldenFixture {
    presets: Vec<GoldenPreset>,
}

#[derive(Debug, Deserialize)]
struct GoldenPreset {
    id: String,
    current_age: u16,
    coast_target_age: u16,
    monthly_expense: i64,
    current_corpus: i64,
    monthly_savings: i64,
    yearly_step_up_pct: Decimal,
    expected_return_pct: Decimal,
    expected_inflation_pct: Decimal,
    multiplier: Decimal,
    max_years: u16,
    expected: GoldenExpected,
}

#[derive(Debug, Deserialize)]
struct GoldenExpected {
    base_fire_target_today: i64,
    years_to_fire: Decimal,
    months_to_fire: u32,
    fire_age: Decimal,
    fire_corpus_at_achieve: Decimal,
    target_nominal_at_achieve: Decimal,
    coast_years_remaining: u16,
    coast_fire_target: Decimal,
    y5_corpus_nominal: Decimal,
    y5_invested_capital: Decimal,
    y5_fire_reached: bool,
    y10_corpus_nominal: Decimal,
    y10_fire_target_nominal: Decimal,
    y10_fire_reached: bool,
    y35_corpus_nominal: Decimal,
    y35_invested_capital: Decimal,
}

fn preset_to_config(p: &GoldenPreset) -> FirePlanConfig {
    FirePlanConfig {
        currency: Currency::Inr,
        current_age: p.current_age,
        coast_target_age: p.coast_target_age,
        expenses: ExpenseInputMode::QuickTotal(Money::inr(Decimal::from(p.monthly_expense))),
        multiplier: p.multiplier,
        current_corpus: Money::inr(Decimal::from(p.current_corpus)),
        monthly_savings: Money::inr(Decimal::from(p.monthly_savings)),
        yearly_step_up_pct: p.yearly_step_up_pct,
        expected_return_pct: p.expected_return_pct,
        expected_inflation_pct: p.expected_inflation_pct,
        tax_drag: None,
        max_years: p.max_years,
    }
}

fn default_itemized_config() -> FirePlanConfig {
    let items = vec![
        ExpenseItem {
            id: "rent".into(),
            name: "Housing".into(),
            category: ExpenseCategory::Essentials,
            monthly_amount: Money::inr(Decimal::from(45_000)),
        },
        ExpenseItem {
            id: "groceries".into(),
            name: "Groceries".into(),
            category: ExpenseCategory::Essentials,
            monthly_amount: Money::inr(Decimal::from(25_000)),
        },
        ExpenseItem {
            id: "utilities".into(),
            name: "Utilities".into(),
            category: ExpenseCategory::Essentials,
            monthly_amount: Money::inr(Decimal::from(8_000)),
        },
        ExpenseItem {
            id: "education".into(),
            name: "Education".into(),
            category: ExpenseCategory::Essentials,
            monthly_amount: Money::inr(Decimal::from(20_000)),
        },
        ExpenseItem {
            id: "healthcare".into(),
            name: "Healthcare".into(),
            category: ExpenseCategory::Security,
            monthly_amount: Money::inr(Decimal::from(10_000)),
        },
        ExpenseItem {
            id: "transport".into(),
            name: "Transport".into(),
            category: ExpenseCategory::Essentials,
            monthly_amount: Money::inr(Decimal::from(12_000)),
        },
        ExpenseItem {
            id: "lifestyle".into(),
            name: "Lifestyle".into(),
            category: ExpenseCategory::Discretionary,
            monthly_amount: Money::inr(Decimal::from(15_000)),
        },
        ExpenseItem {
            id: "vacation".into(),
            name: "Vacation".into(),
            category: ExpenseCategory::Discretionary,
            monthly_amount: Money::inr(Decimal::from(15_000)),
        },
        ExpenseItem {
            id: "buffer".into(),
            name: "Buffer".into(),
            category: ExpenseCategory::Security,
            monthly_amount: Money::inr(Decimal::from(10_000)),
        },
    ];
    FirePlanConfig {
        currency: Currency::Inr,
        current_age: 35,
        coast_target_age: 58,
        expenses: ExpenseInputMode::Itemized(items),
        multiplier: Decimal::from(30),
        current_corpus: Money::inr(Decimal::from(15_000_000)),
        monthly_savings: Money::inr(Decimal::from(200_000)),
        yearly_step_up_pct: Decimal::from(8),
        expected_return_pct: Decimal::from(12),
        expected_inflation_pct: Decimal::from(6),
        tax_drag: None,
        max_years: 35,
    }
}

#[test]
fn golden_fireapp_presets_match_decimal_engine() {
    let raw = include_str!("golden/fire_golden.json");
    let fixture: GoldenFixture = serde_json::from_str(raw).expect("valid golden JSON");
    assert_eq!(fixture.presets.len(), 4);

    // JS reference fixture rounds every step/result to nearest rupee; Decimal rounds to 2dp.
    let rounding_slack = Decimal::from(2);

    for preset in &fixture.presets {
        let cfg = preset_to_config(preset);
        let report = simulate_fire_runway(&cfg).expect("preset simulation succeeds");

        assert_eq!(
            report.base_fire_target_today.amount,
            Decimal::from(preset.expected.base_fire_target_today),
            "preset {} base target",
            preset.id
        );
        assert_eq!(
            report.crossover.months_to_fire,
            Some(preset.expected.months_to_fire),
            "preset {} months_to_fire",
            preset.id
        );
        assert_eq!(
            report.crossover.years_to_fire,
            Some(preset.expected.years_to_fire),
            "preset {} years_to_fire",
            preset.id
        );
        assert_eq!(
            report.crossover.fire_age,
            Some(preset.expected.fire_age),
            "preset {} fire_age",
            preset.id
        );
        assert!(
            (report.crossover.fire_corpus_at_achieve.unwrap().amount
                - preset.expected.fire_corpus_at_achieve)
                .abs()
                <= rounding_slack,
            "preset {} fire_corpus_at_achieve diff",
            preset.id
        );
        assert!(
            (report.crossover.target_nominal_at_achieve.unwrap().amount
                - preset.expected.target_nominal_at_achieve)
                .abs()
                <= rounding_slack,
            "preset {} target_nominal_at_achieve diff",
            preset.id
        );

        let y5 = &report.points[5];
        assert!(
            (y5.corpus_nominal.amount - preset.expected.y5_corpus_nominal).abs() <= rounding_slack
        );
        assert!(
            (y5.invested_capital_nominal.amount - preset.expected.y5_invested_capital).abs()
                <= rounding_slack
        );
        assert_eq!(y5.is_fire_reached, preset.expected.y5_fire_reached);

        let y10 = &report.points[10];
        assert!(
            (y10.corpus_nominal.amount - preset.expected.y10_corpus_nominal).abs()
                <= rounding_slack
        );
        assert!(
            (y10.fire_target_nominal.amount - preset.expected.y10_fire_target_nominal).abs()
                <= rounding_slack
        );
        assert_eq!(y10.is_fire_reached, preset.expected.y10_fire_reached);

        let y35 = &report.points[35];
        assert!(
            (y35.corpus_nominal.amount - preset.expected.y35_corpus_nominal).abs()
                <= Decimal::from(50),
            "preset {} y35 corpus diff: rust={} js={}",
            preset.id,
            y35.corpus_nominal.amount,
            preset.expected.y35_corpus_nominal
        );
        assert!(
            (y35.invested_capital_nominal.amount - preset.expected.y35_invested_capital).abs()
                <= Decimal::from(5),
            "preset {} y35 invested diff: rust={} js={}",
            preset.id,
            y35.invested_capital_nominal.amount,
            preset.expected.y35_invested_capital
        );

        let coast = report
            .archetypes
            .iter()
            .find(|a| a.id == FireArchetypeId::Coast)
            .expect("Coast archetype present");
        assert_eq!(
            coast.coast_years_remaining,
            Some(preset.expected.coast_years_remaining)
        );
        assert!(
            (coast.target_corpus.amount - preset.expected.coast_fire_target).abs()
                <= rounding_slack,
            "preset {} coast target diff",
            preset.id
        );
    }
}

#[test]
fn itemized_expenses_sum_matches_quick_total_160k() {
    let cfg = default_itemized_config();
    let monthly = cfg.effective_monthly_expense().unwrap();
    assert_eq!(monthly, Money::inr(Decimal::from(160_000)));
    let archetypes = calculate_fire_archetypes(&cfg).unwrap();
    assert_eq!(archetypes.len(), 5);
    let std_card = archetypes
        .iter()
        .find(|a| a.id == FireArchetypeId::Standard)
        .unwrap();
    assert_eq!(
        std_card.target_corpus,
        Money::inr(Decimal::from(57_600_000))
    );
    let lean_card = archetypes
        .iter()
        .find(|a| a.id == FireArchetypeId::Lean)
        .unwrap();
    assert_eq!(lean_card.multiplier, Decimal::from(22));
    assert_eq!(lean_card.monthly_budget, Money::inr(Decimal::from(120_000)));
    let fat_card = archetypes
        .iter()
        .find(|a| a.id == FireArchetypeId::Fat)
        .unwrap();
    assert_eq!(fat_card.multiplier, Decimal::from(40));
    assert_eq!(fat_card.monthly_budget, Money::inr(Decimal::from(216_000)));
    let barista = archetypes
        .iter()
        .find(|a| a.id == FireArchetypeId::Barista)
        .unwrap();
    assert_eq!(barista.monthly_budget, Money::inr(Decimal::from(96_000)));
}

#[test]
fn india_finance_act_2025_tax_drag_reduces_net_return_above_exemption() {
    let mut cfg = default_itemized_config();
    let gross = effective_net_annual_return_pct(&cfg);
    assert_eq!(gross, Decimal::from(12));

    // ₹1.5 Cr @ 12% = ₹18L gross gain; 50% realized = ₹9L; 20% STCG (₹1.8L * 20% = ₹36k)
    // + 80% LTCG (₹7.2L - ₹1.25L exemption = ₹5.95L * 12.5% = ₹74,375)
    // Total tax = ₹1,10,375 on ₹1.5 Cr => 0.7358333...% drag
    cfg.tax_drag = Some(TaxDragConfig::india_finance_act_2025(
        Decimal::from(50),
        Decimal::from(20),
    ));
    let net = effective_net_annual_return_pct(&cfg);
    assert!(net < gross);
    assert!(net > Decimal::from(11));

    let report_gross = simulate_fire_runway(&default_itemized_config()).unwrap();
    let report_net = simulate_fire_runway(&cfg).unwrap();
    assert!(report_net.effective_return_pct < report_gross.effective_return_pct);
    assert!(
        report_net.points.last().unwrap().corpus_nominal.amount
            < report_gross.points.last().unwrap().corpus_nominal.amount
    );
}

#[test]
fn post_fire_cashflow_3_bucket_calculations_match_formula() {
    let corpus = Money::inr(Decimal::from(54_000_000)); // ₹5.4 Cr
    let monthly_exp = Money::inr(Decimal::from(150_000)); // ₹1.5L/mo
    let mix = CashflowBucketConfig::default(); // 65% eq @ 1.2%, 25% debt @ 6.8%, 10% theta @ 1.5%/mo

    let cf = simulate_post_fire_cashflow(corpus, monthly_exp, &mix).unwrap();
    // Equity: 5.4Cr * 0.65 * 0.012 / 12 = 35,100
    assert_eq!(cf.monthly_equity_income, Money::inr(Decimal::from(35_100)));
    // Debt: 5.4Cr * 0.25 * 0.068 / 12 = 76,500
    assert_eq!(cf.monthly_debt_income, Money::inr(Decimal::from(76_500)));
    // Theta: 5.4Cr * 0.10 * 0.015 = 81,000
    assert_eq!(cf.monthly_theta_income, Money::inr(Decimal::from(81_000)));
    // Total passive = 192,600 -> Surplus = +42,600
    assert_eq!(cf.total_passive_monthly, Money::inr(Decimal::from(192_600)));
    assert_eq!(cf.surplus_or_deficit, Money::inr(Decimal::from(42_600)));
    assert_eq!(cf.annual_principal_drawdown_pct, Decimal::ZERO);
    assert!(cf.expense_coverage_pct > Decimal::from(100));
}

#[test]
fn sor_stress_is_seed_deterministic_and_monotonic_with_initial_corpus() {
    let cfg = default_itemized_config();
    let sor = SorStressConfig {
        seed: 99,
        post_fire_years: 30,
        annual_volatility_pct: Decimal::from(16),
        iterations: 500,
        historical_annual_returns: None,
    };

    let rep_low =
        simulate_fire_runway_with_extras(&cfg, Some(&CashflowBucketConfig::default()), Some(&sor))
            .unwrap();
    let rep_low_2 =
        simulate_fire_runway_with_extras(&cfg, Some(&CashflowBucketConfig::default()), Some(&sor))
            .unwrap();
    assert_eq!(rep_low.sor_stress, rep_low_2.sor_stress);
    assert!(rep_low.post_fire_cashflow.is_some());

    let s_base = rep_low.sor_stress.unwrap();
    let s_rich = run_sor_stress(&cfg, Money::inr(Decimal::from(120_000_000)), &sor).unwrap();
    assert!(s_rich.survival_probability_pct >= s_base.survival_probability_pct);
    assert!(s_rich.p50_terminal_real.amount >= s_base.p50_terminal_real.amount);
}

#[test]
fn sor_bootstrap_mode_works_with_historical_returns() {
    let cfg = default_itemized_config();
    let sor = SorStressConfig {
        seed: 7,
        post_fire_years: 10,
        annual_volatility_pct: Decimal::from(15),
        iterations: 200,
        historical_annual_returns: Some(vec![
            Decimal::new(-20, 2),
            Decimal::new(15, 2),
            Decimal::new(18, 2),
        ]),
    };
    let res = run_sor_stress(&cfg, Money::inr(Decimal::from(60_000_000)), &sor).unwrap();
    assert!(res.survival_probability_pct <= Decimal::from(100));
    assert!(res.p10_terminal_real.amount <= res.p50_terminal_real.amount);
    assert!(res.p50_terminal_real.amount <= res.p90_terminal_real.amount);
}

#[test]
fn validation_rejects_currency_mismatch_and_invalid_bounds() {
    let mut cfg = default_itemized_config();
    cfg.current_corpus = Money::new(Decimal::from(100_000), Currency::Usd);
    assert!(matches!(
        simulate_fire_runway(&cfg),
        Err(FireError::CurrencyMismatch {
            expected: Currency::Inr,
            found: Currency::Usd
        })
    ));

    let mut cfg2 = default_itemized_config();
    cfg2.multiplier = Decimal::from(15);
    assert!(matches!(
        simulate_fire_runway(&cfg2),
        Err(FireError::InvalidInput(_))
    ));

    let bad_mix = CashflowBucketConfig {
        equity_pct: Decimal::from(80),
        ..Default::default()
    };
    assert!(matches!(
        bad_mix.validate(),
        Err(FireError::InvalidInput(_))
    ));

    assert!(matches!(
        calculate_coast_fire_corpus(
            Money::inr(Decimal::from(-1)),
            10,
            Decimal::from(12),
            Decimal::from(6)
        ),
        Err(FireError::InvalidInput(_))
    ));
}

proptest! {
    #[test]
    fn real_corpus_never_exceeds_nominal_under_non_negative_inflation(
        corpus in 100_000_i64..10_000_000_i64,
        savings in 10_000_i64..500_000_i64,
        expense in 20_000_i64..200_000_i64,
        inf_pct in 0_i64..12_i64,
        ret_pct in 4_i64..18_i64,
        years in 1_u16..30_u16,
    ) {
        let cfg = FirePlanConfig {
            currency: Currency::Inr,
            current_age: 30,
            coast_target_age: 58,
            expenses: ExpenseInputMode::QuickTotal(Money::inr(Decimal::from(expense))),
            multiplier: Decimal::from(30),
            current_corpus: Money::inr(Decimal::from(corpus)),
            monthly_savings: Money::inr(Decimal::from(savings)),
            yearly_step_up_pct: Decimal::from(5),
            expected_return_pct: Decimal::from(ret_pct),
            expected_inflation_pct: Decimal::from(inf_pct),
            tax_drag: None,
            max_years: years,
        };
        let rep = simulate_fire_runway(&cfg).unwrap();
        prop_assert_eq!(rep.points.len(), usize::from(years) + 1);
        for pt in &rep.points {
            prop_assert!(pt.corpus_real.amount <= pt.corpus_nominal.amount + Decimal::new(1, 2));
            prop_assert!(pt.fire_target_nominal.amount >= pt.fire_target_real.amount - Decimal::new(1, 2));
        }
    }
}
