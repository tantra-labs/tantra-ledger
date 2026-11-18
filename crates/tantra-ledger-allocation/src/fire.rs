//! Pure deterministic FIRE planning, compounding, archetype & SoRR engine.
//!
//! Incorporates the mathematical kernel of `Elicherla01/fireapp` ("FIRE Works")
//! into L1 (`tantra-ledger-allocation`) with strict adherence to `tantra-labs`
//! invariants:
//! - Zero floating-point arithmetic (`rust_decimal::Decimal` + `tantra_core::Money`).
//! - Zero I/O, zero network, zero async, zero system clock queries (`wasm32`-clean).
//! - Deterministic seeded Sequence-of-Returns Risk (SoRR) stress testing via
//!   [`tantra_core::analytics::distribution::SplitMix64`] and an exact-decimal
//!   256-bin symmetric normal quantile table.

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use rust_decimal::RoundingStrategy;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Currency, Money};
use tantra_core::analytics::distribution::SplitMix64;

const SCALE_MONEY_DP: u32 = 2;
const SCALE_PCT_DP: u32 = 2;
const HUNDRED: u64 = 100;
const TWELVE: u64 = 12;

/// Symmetric positive half of a 256-bin stratified N(0, 1) inverse-CDF table,
/// scaled by `1e8` (`k = 0..128` at cumulative probability `0.5 + (2k + 1) / 512`).
const NORMAL_POS_QUANTILES_1E8: [i64; 128] = [
    489_578,
    1_468_780,
    2_448_124,
    3_427_702,
    4_407_609,
    5_387_940,
    6_368_789,
    7_350_251,
    8_332_421,
    9_315_396,
    10_299_272,
    11_284_146,
    12_270_115,
    13_257_279,
    14_245_737,
    15_235_588,
    16_226_935,
    17_219_879,
    18_214_524,
    19_210_973,
    20_209_335,
    21_209_714,
    22_212_221,
    23_216_965,
    24_224_058,
    25_233_615,
    26_245_750,
    27_260_580,
    28_278_227,
    29_298_810,
    30_322_454,
    31_349_285,
    32_379_433,
    33_413_028,
    34_450_206,
    35_491_102,
    36_535_859,
    37_584_619,
    38_637_529,
    39_694_740,
    40_756_407,
    41_822_687,
    42_893_744,
    43_969_745,
    45_050_861,
    46_137_268,
    47_229_148,
    48_326_688,
    49_430_081,
    50_539_526,
    51_655_226,
    52_777_394,
    53_906_247,
    55_042_012,
    56_184_923,
    57_335_220,
    58_493_154,
    59_658_985,
    60_832_982,
    62_015_423,
    63_206_600,
    64_406_814,
    65_616_378,
    66_835_620,
    68_064_879,
    69_304_510,
    70_554_884,
    71_816_387,
    73_089_425,
    74_374_419,
    75_671_813,
    76_982_072,
    78_305_681,
    79_643_154,
    80_995_028,
    82_361_870,
    83_744_274,
    85_142_871,
    86_558_324,
    87_991_335,
    89_442_648,
    90_913_049,
    92_403_374,
    93_914_510,
    95_447_403,
    97_003_058,
    98_582_550,
    100_187_028,
    101_817_721,
    103_475_948,
    105_163_128,
    106_880_788,
    108_630_574,
    110_414_268,
    112_233_801,
    114_091_271,
    115_988_962,
    117_929_369,
    119_915_225,
    121_949_532,
    124_035_599,
    126_177_086,
    128_378_055,
    130_643_035,
    132_977_095,
    135_385_936,
    137_876_004,
    140_454_625,
    143_130_176,
    145_912_303,
    148_812_190,
    151_842_914,
    155_019_904,
    158_361_548,
    161_890_014,
    165_632_387,
    169_622_251,
    173_901_997,
    178_526_249,
    183_567_154,
    189_122_924,
    195_332_371,
    202_401_362,
    210_655_409,
    220_657_522,
    233_523_304,
    252_050_222,
    288_563_491,
];

/// Errors produced by validating or evaluating FIRE plans and cashflow projections.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FireError {
    #[error("invalid FIRE parameter: {0}")]
    InvalidInput(String),
    #[error("currency mismatch: expected {expected}, found {found}")]
    CurrencyMismatch { expected: Currency, found: Currency },
    #[error("numeric overflow in FIRE compounding calculation")]
    Overflow,
}

/// Expense bucket classification mirroring `fireapp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpenseCategory {
    Essentials,
    Security,
    Discretionary,
}

/// Single monthly line-item expense.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpenseItem {
    pub id: String,
    pub name: String,
    pub category: ExpenseCategory,
    pub monthly_amount: Money,
}

/// Monthly expense specification mode (itemized breakdown vs lump sum).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum ExpenseInputMode {
    Itemized(Vec<ExpenseItem>),
    QuickTotal(Money),
}

/// Optional annualized capital-gains tax-drag model applied on top of nominal return.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxDragConfig {
    /// Short-term capital gains tax rate in percentage points (e.g., `20` for India Equity STCG).
    pub stcg_rate_pct: Decimal,
    /// Long-term capital gains tax rate in percentage points (e.g., `12.5` for India Equity LTCG).
    pub ltcg_rate_pct: Decimal,
    /// Annual LTCG exemption threshold (e.g., ₹1,25,000 under Finance Act 2025).
    pub annual_ltcg_exemption: Money,
    /// Fraction of portfolio turnover realized each year in percentage points (`0..=100`).
    pub realized_turnover_pct: Decimal,
    /// Share of realized gains taxed at STCG instead of LTCG in percentage points (`0..=100`).
    pub short_term_share_pct: Decimal,
}

impl TaxDragConfig {
    /// India Finance Act 2025 equity taxation preset (20% STCG, 12.5% LTCG above ₹1,25,000).
    pub fn india_finance_act_2025(
        realized_turnover_pct: Decimal,
        short_term_share_pct: Decimal,
    ) -> Self {
        Self {
            stcg_rate_pct: Decimal::new(20, 0),
            ltcg_rate_pct: Decimal::new(125, 1), // 12.5%
            annual_ltcg_exemption: Money::inr(Decimal::new(125_000, 0)),
            realized_turnover_pct,
            short_term_share_pct,
        }
    }
}

/// Canonical FIRE simulation request/configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirePlanConfig {
    pub currency: Currency,
    pub current_age: u16,
    pub coast_target_age: u16,
    pub expenses: ExpenseInputMode,
    /// Corpus-to-annual-expense multiple (`20..=50`, default `30` for India).
    pub multiplier: Decimal,
    pub current_corpus: Money,
    pub monthly_savings: Money,
    /// Annual SIP step-up percentage (`>= 0`, e.g. `10` for 10%).
    pub yearly_step_up_pct: Decimal,
    /// Expected gross annual return percentage (e.g. `12` for 12%).
    pub expected_return_pct: Decimal,
    /// Expected annual CPI inflation percentage (e.g. `6` for 6%).
    pub expected_inflation_pct: Decimal,
    #[serde(default)]
    pub tax_drag: Option<TaxDragConfig>,
    /// Horizon in years (`1..=60`, default `35`).
    pub max_years: u16,
}

impl FirePlanConfig {
    /// Validates currency consistency, bounds, and non-negative monetary invariants.
    pub fn validate(&self) -> Result<(), FireError> {
        if self.current_corpus.currency != self.currency {
            return Err(FireError::CurrencyMismatch {
                expected: self.currency,
                found: self.current_corpus.currency,
            });
        }
        if self.monthly_savings.currency != self.currency {
            return Err(FireError::CurrencyMismatch {
                expected: self.currency,
                found: self.monthly_savings.currency,
            });
        }
        if self.current_corpus.amount < Decimal::ZERO {
            return Err(FireError::InvalidInput(
                "current_corpus cannot be negative".into(),
            ));
        }
        if self.monthly_savings.amount < Decimal::ZERO {
            return Err(FireError::InvalidInput(
                "monthly_savings cannot be negative".into(),
            ));
        }
        if self.multiplier < Decimal::from(20) || self.multiplier > Decimal::from(50) {
            return Err(FireError::InvalidInput(
                "multiplier must be between 20 and 50".into(),
            ));
        }
        if self.max_years == 0 || self.max_years > 60 {
            return Err(FireError::InvalidInput(
                "max_years must be between 1 and 60".into(),
            ));
        }
        if self.current_age > 100 {
            return Err(FireError::InvalidInput("current_age must be <= 100".into()));
        }
        if self.coast_target_age > 120 {
            return Err(FireError::InvalidInput(
                "coast_target_age must be <= 120".into(),
            ));
        }
        if self.yearly_step_up_pct < Decimal::ZERO {
            return Err(FireError::InvalidInput(
                "yearly_step_up_pct cannot be negative".into(),
            ));
        }
        let neg_hundred = -Decimal::from(HUNDRED);
        if self.expected_return_pct <= neg_hundred {
            return Err(FireError::InvalidInput(
                "expected_return_pct must be greater than -100".into(),
            ));
        }
        if self.expected_inflation_pct <= neg_hundred {
            return Err(FireError::InvalidInput(
                "expected_inflation_pct must be greater than -100".into(),
            ));
        }

        let monthly_exp = match &self.expenses {
            ExpenseInputMode::QuickTotal(m) => {
                if m.currency != self.currency {
                    return Err(FireError::CurrencyMismatch {
                        expected: self.currency,
                        found: m.currency,
                    });
                }
                if m.amount < Decimal::ZERO {
                    return Err(FireError::InvalidInput(
                        "monthly expense cannot be negative".into(),
                    ));
                }
                m.amount
            }
            ExpenseInputMode::Itemized(items) => {
                let mut sum = Decimal::ZERO;
                for item in items {
                    if item.monthly_amount.currency != self.currency {
                        return Err(FireError::CurrencyMismatch {
                            expected: self.currency,
                            found: item.monthly_amount.currency,
                        });
                    }
                    if item.monthly_amount.amount < Decimal::ZERO {
                        return Err(FireError::InvalidInput(format!(
                            "expense item '{}' cannot be negative",
                            item.id
                        )));
                    }
                    sum += item.monthly_amount.amount;
                }
                sum
            }
        };

        if monthly_exp <= Decimal::ZERO {
            return Err(FireError::InvalidInput(
                "effective monthly expense must be strictly positive".into(),
            ));
        }

        if let Some(tax) = &self.tax_drag {
            if tax.annual_ltcg_exemption.currency != self.currency {
                return Err(FireError::CurrencyMismatch {
                    expected: self.currency,
                    found: tax.annual_ltcg_exemption.currency,
                });
            }
            let zero = Decimal::ZERO;
            let hundred = Decimal::from(HUNDRED);
            if tax.stcg_rate_pct < zero
                || tax.stcg_rate_pct > hundred
                || tax.ltcg_rate_pct < zero
                || tax.ltcg_rate_pct > hundred
                || tax.realized_turnover_pct < zero
                || tax.realized_turnover_pct > hundred
                || tax.short_term_share_pct < zero
                || tax.short_term_share_pct > hundred
                || tax.annual_ltcg_exemption.amount < zero
            {
                return Err(FireError::InvalidInput(
                    "tax_drag rates/turnover/share must lie in [0, 100] with non-negative exemption"
                        .into(),
                ));
            }
        }

        Ok(())
    }

    /// Computes effective monthly expenses in the plan currency.
    pub fn effective_monthly_expense(&self) -> Result<Money, FireError> {
        self.validate()?;
        let amount = match &self.expenses {
            ExpenseInputMode::QuickTotal(m) => m.amount,
            ExpenseInputMode::Itemized(items) => items
                .iter()
                .map(|i| i.monthly_amount.amount)
                .fold(Decimal::ZERO, |acc, x| acc + x),
        };
        Ok(Money::new(amount, self.currency))
    }
}

/// Annual projection point for nominal and inflation-adjusted ("today's money") paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YearlyProjectionPoint {
    pub year: u16,
    pub age: u16,
    pub corpus_nominal: Money,
    pub corpus_real: Money,
    pub invested_capital_nominal: Money,
    pub total_gains_nominal: Money,
    pub monthly_sip: Money,
    pub fire_target_nominal: Money,
    pub fire_target_real: Money,
    pub is_fire_reached: bool,
}

/// Crossover point where simulated corpus first meets/exceeds inflation-adjusted target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireCrossoverMilestone {
    pub years_to_fire: Option<Decimal>,
    pub months_to_fire: Option<u32>,
    pub fire_age: Option<Decimal>,
    pub fire_corpus_at_achieve: Option<Money>,
    pub target_nominal_at_achieve: Option<Money>,
}

/// Five FIRE lifestyle & freedom archetypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FireArchetypeId {
    Lean,
    Standard,
    Fat,
    Coast,
    Barista,
}

/// Evaluation card for one FIRE archetype.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireArchetypeCard {
    pub id: FireArchetypeId,
    pub multiplier: Decimal,
    pub swr_pct: Decimal,
    pub monthly_budget: Money,
    pub target_corpus: Money,
    pub shortfall: Money,
    pub progress_pct: Decimal,
    pub coast_years_remaining: Option<u16>,
    pub achieved_today: bool,
}

/// Post-FIRE 3-bucket paycheck allocation parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashflowBucketConfig {
    pub equity_pct: Decimal,
    pub debt_pct: Decimal,
    pub theta_margin_pct: Decimal,
    /// Annual dividend/yield % on the equity bucket (default `1.2%`).
    pub equity_annual_yield_pct: Decimal,
    /// Annual post-tax yield % on the debt/arbitrage runway (default `6.8%`).
    pub debt_annual_yield_pct: Decimal,
    /// Monthly options theta yield % on dedicated options margin (default `1.5%/mo`).
    pub theta_monthly_yield_pct: Decimal,
}

impl Default for CashflowBucketConfig {
    fn default() -> Self {
        Self {
            equity_pct: Decimal::from(65),
            debt_pct: Decimal::from(25),
            theta_margin_pct: Decimal::from(10),
            equity_annual_yield_pct: Decimal::new(12, 1), // 1.2%
            debt_annual_yield_pct: Decimal::new(68, 1),   // 6.8%
            theta_monthly_yield_pct: Decimal::new(15, 1), // 1.5%/mo
        }
    }
}

impl CashflowBucketConfig {
    /// Validates bucket weights are non-negative and sum cleanly to 100%.
    pub fn validate(&self) -> Result<(), FireError> {
        let zero = Decimal::ZERO;
        let hundred = Decimal::from(HUNDRED);
        if self.equity_pct < zero || self.debt_pct < zero || self.theta_margin_pct < zero {
            return Err(FireError::InvalidInput(
                "cashflow bucket percentages cannot be negative".into(),
            ));
        }
        let sum = self.equity_pct + self.debt_pct + self.theta_margin_pct;
        let tol = Decimal::new(1, 4); // 0.0001%
        if (sum - hundred).abs() > tol {
            return Err(FireError::InvalidInput(format!(
                "cashflow bucket weights must sum to 100 (got {sum})"
            )));
        }
        if self.equity_annual_yield_pct < zero
            || self.debt_annual_yield_pct < zero
            || self.theta_monthly_yield_pct < zero
        {
            return Err(FireError::InvalidInput(
                "cashflow yield rates cannot be negative".into(),
            ));
        }
        Ok(())
    }
}

/// Simulated monthly passive income breakdown across the 3 FIRE buckets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostFireCashflowReport {
    pub monthly_equity_income: Money,
    pub monthly_debt_income: Money,
    pub monthly_theta_income: Money,
    pub total_passive_monthly: Money,
    pub monthly_expense_target: Money,
    pub surplus_or_deficit: Money,
    pub expense_coverage_pct: Decimal,
    pub net_annual_yield_pct: Decimal,
    pub annual_principal_drawdown_pct: Decimal,
}

/// Configuration for deterministic Sequence-of-Returns Risk (SoRR) post-FIRE stress test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SorStressConfig {
    pub seed: u64,
    pub post_fire_years: u16,
    pub annual_volatility_pct: Decimal,
    pub iterations: u32,
    /// Optional empirical annual return stream (decimal fractions, e.g. `0.12` for +12%).
    /// When provided and non-empty, paths bootstrap with replacement from these returns;
    /// otherwise draws stratified symmetric Gaussian shocks via `SplitMix64`.
    #[serde(default)]
    pub historical_annual_returns: Option<Vec<Decimal>>,
}

impl Default for SorStressConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            post_fire_years: 30,
            annual_volatility_pct: Decimal::from(15),
            iterations: 1_000,
            historical_annual_returns: None,
        }
    }
}

impl SorStressConfig {
    pub fn validate(&self) -> Result<(), FireError> {
        if self.post_fire_years == 0 || self.post_fire_years > 60 {
            return Err(FireError::InvalidInput(
                "post_fire_years must be between 1 and 60".into(),
            ));
        }
        if self.iterations == 0 || self.iterations > 10_000 {
            return Err(FireError::InvalidInput(
                "iterations must be between 1 and 10000".into(),
            ));
        }
        if self.annual_volatility_pct < Decimal::ZERO {
            return Err(FireError::InvalidInput(
                "annual_volatility_pct cannot be negative".into(),
            ));
        }
        if let Some(hist) = &self.historical_annual_returns
            && hist.is_empty()
        {
            return Err(FireError::InvalidInput(
                "historical_annual_returns cannot be empty when specified".into(),
            ));
        }
        Ok(())
    }
}

/// Summary distribution metrics from a seeded SoRR decumulation stress test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SorStressResult {
    pub survival_probability_pct: Decimal,
    pub p10_terminal_real: Money,
    pub p50_terminal_real: Money,
    pub p90_terminal_real: Money,
    pub worst_depletion_year: Option<u16>,
}

/// Unified FIRE simulation output payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireSimulationReport {
    pub effective_monthly_expense: Money,
    pub effective_annual_expense: Money,
    pub base_fire_target_today: Money,
    pub swr_pct: Decimal,
    pub effective_return_pct: Decimal,
    pub crossover: FireCrossoverMilestone,
    pub points: Vec<YearlyProjectionPoint>,
    pub archetypes: Vec<FireArchetypeCard>,
    pub post_fire_cashflow: Option<PostFireCashflowReport>,
    pub sor_stress: Option<SorStressResult>,
}

fn round_money(amount: Decimal) -> Decimal {
    amount.round_dp_with_strategy(SCALE_MONEY_DP, RoundingStrategy::MidpointAwayFromZero)
}

fn round_pct(val: Decimal) -> Decimal {
    val.round_dp_with_strategy(SCALE_PCT_DP, RoundingStrategy::MidpointAwayFromZero)
}

fn round_1dp(val: Decimal) -> Decimal {
    val.round_dp_with_strategy(1, RoundingStrategy::MidpointAwayFromZero)
}

/// Computes effective net annual return percentage after optional tax drag.
///
/// When `tax_drag` is `None`, returns `config.expected_return_pct` unchanged.
pub fn effective_net_annual_return_pct(config: &FirePlanConfig) -> Decimal {
    let gross = config.expected_return_pct;
    let Some(tax) = &config.tax_drag else {
        return gross;
    };
    if gross <= Decimal::ZERO {
        return gross;
    }

    let hundred = Decimal::from(HUNDRED);
    let turnover_frac = tax.realized_turnover_pct / hundred;
    let st_frac = tax.short_term_share_pct / hundred;
    let lt_frac = Decimal::ONE - st_frac;

    let gross_gain = config.current_corpus.amount * (gross / hundred);
    let realized_gain = (gross_gain * turnover_frac).max(Decimal::ZERO);
    let st_gain = realized_gain * st_frac;
    let lt_gain = realized_gain * lt_frac;
    let taxable_lt = (lt_gain - tax.annual_ltcg_exemption.amount).max(Decimal::ZERO);

    let tax_amount =
        st_gain * (tax.stcg_rate_pct / hundred) + taxable_lt * (tax.ltcg_rate_pct / hundred);

    // Express drag as annual basis reduction vs corpus (or turnover-weighted rate when corpus == 0).
    let drag_pct = if config.current_corpus.amount > Decimal::ZERO {
        (tax_amount / config.current_corpus.amount) * hundred
    } else {
        let blended_rate_pct = st_frac * tax.stcg_rate_pct + lt_frac * tax.ltcg_rate_pct;
        gross * turnover_frac * (blended_rate_pct / hundred)
    };

    (gross - drag_pct).max(Decimal::ZERO)
}

/// Simulates the multi-year monthly-compounded FIRE runway using the canonical parameters.
pub fn simulate_fire_runway(config: &FirePlanConfig) -> Result<FireSimulationReport, FireError> {
    simulate_fire_runway_with_extras(config, None, None)
}

/// Simulates FIRE runway plus optional 3-bucket post-FIRE cashflow & SoRR stress test.
pub fn simulate_fire_runway_with_extras(
    config: &FirePlanConfig,
    cashflow_mix: Option<&CashflowBucketConfig>,
    sor_config: Option<&SorStressConfig>,
) -> Result<FireSimulationReport, FireError> {
    config.validate()?;
    if let Some(mix) = cashflow_mix {
        mix.validate()?;
    }
    if let Some(sor) = sor_config {
        sor.validate()?;
    }

    let effective_monthly = config.effective_monthly_expense()?;
    let effective_annual = Money::new(
        round_money(effective_monthly.amount * Decimal::from(TWELVE)),
        config.currency,
    );
    let base_fire_target = round_money(effective_annual.amount * config.multiplier);
    let hundred = Decimal::from(HUNDRED);
    let swr_pct = round_pct(hundred / config.multiplier);
    let net_return_pct = effective_net_annual_return_pct(config);

    let monthly_nominal_return = net_return_pct / hundred / Decimal::from(TWELVE);
    let step_up_multiplier = Decimal::ONE + (config.yearly_step_up_pct / hundred);
    let inflation_multiplier = Decimal::ONE + (config.expected_inflation_pct / hundred);
    let monthly_inflation_factor = inflation_multiplier
        .checked_powd(Decimal::ONE / Decimal::from(TWELVE))
        .ok_or(FireError::Overflow)?;

    let mut corpus = config.current_corpus.amount;
    let mut invested_capital = config.current_corpus.amount;
    let mut current_sip = config.monthly_savings.amount;

    let mut points = Vec::with_capacity(usize::from(config.max_years) + 1);
    let mut years_to_fire: Option<Decimal> = None;
    let mut months_to_fire: Option<u32> = None;
    let mut fire_age: Option<Decimal> = None;
    let mut fire_corpus_at_achieve: Option<Money> = None;
    let mut target_nominal_at_achieve: Option<Money> = None;

    let y0_gains = (corpus - invested_capital).max(Decimal::ZERO);
    let y0_reached = corpus >= base_fire_target;
    points.push(YearlyProjectionPoint {
        year: 0,
        age: config.current_age,
        corpus_nominal: Money::new(round_money(corpus), config.currency),
        corpus_real: Money::new(round_money(corpus), config.currency),
        invested_capital_nominal: Money::new(round_money(invested_capital), config.currency),
        total_gains_nominal: Money::new(round_money(y0_gains), config.currency),
        monthly_sip: Money::new(round_money(current_sip), config.currency),
        fire_target_nominal: Money::new(base_fire_target, config.currency),
        fire_target_real: Money::new(base_fire_target, config.currency),
        is_fire_reached: y0_reached,
    });

    if y0_reached {
        years_to_fire = Some(Decimal::ZERO);
        months_to_fire = Some(0);
        fire_age = Some(Decimal::from(config.current_age));
        fire_corpus_at_achieve = Some(Money::new(round_money(corpus), config.currency));
        target_nominal_at_achieve = Some(Money::new(base_fire_target, config.currency));
    }

    for yr in 1..=config.max_years {
        let prior_year_inf = inflation_multiplier
            .checked_powi(i64::from(yr - 1))
            .ok_or(FireError::Overflow)?;
        let mut intra_inf = prior_year_inf;

        for m in 1..=12_u32 {
            corpus = corpus
                .checked_mul(Decimal::ONE + monthly_nominal_return)
                .and_then(|c| c.checked_add(current_sip))
                .ok_or(FireError::Overflow)?;
            invested_capital = invested_capital
                .checked_add(current_sip)
                .ok_or(FireError::Overflow)?;

            intra_inf = intra_inf
                .checked_mul(monthly_inflation_factor)
                .ok_or(FireError::Overflow)?;
            let current_nominal_target = base_fire_target * intra_inf;

            if corpus >= current_nominal_target && years_to_fire.is_none() {
                let total_months = u32::from(yr - 1) * 12 + m;
                let exact_years = Decimal::from(total_months) / Decimal::from(TWELVE);
                let rounded_years = round_1dp(exact_years);
                let rounded_age = round_1dp(Decimal::from(config.current_age) + rounded_years);

                years_to_fire = Some(rounded_years);
                months_to_fire = Some(total_months);
                fire_age = Some(rounded_age);
                fire_corpus_at_achieve = Some(Money::new(round_money(corpus), config.currency));
                target_nominal_at_achieve = Some(Money::new(
                    round_money(current_nominal_target),
                    config.currency,
                ));
            }
        }

        current_sip = current_sip
            .checked_mul(step_up_multiplier)
            .ok_or(FireError::Overflow)?;

        let year_inf = inflation_multiplier
            .checked_powi(i64::from(yr))
            .ok_or(FireError::Overflow)?;
        let fire_target_nominal = base_fire_target * year_inf;
        let corpus_real = if year_inf.is_zero() {
            corpus
        } else {
            corpus / year_inf
        };
        let gains = (corpus - invested_capital).max(Decimal::ZERO);

        points.push(YearlyProjectionPoint {
            year: yr,
            age: config.current_age + yr,
            corpus_nominal: Money::new(round_money(corpus), config.currency),
            corpus_real: Money::new(round_money(corpus_real), config.currency),
            invested_capital_nominal: Money::new(round_money(invested_capital), config.currency),
            total_gains_nominal: Money::new(round_money(gains), config.currency),
            monthly_sip: Money::new(round_money(current_sip), config.currency),
            fire_target_nominal: Money::new(round_money(fire_target_nominal), config.currency),
            fire_target_real: Money::new(base_fire_target, config.currency),
            is_fire_reached: corpus >= fire_target_nominal,
        });
    }

    let archetypes = calculate_fire_archetypes_with_net(config, net_return_pct)?;
    let target_money = Money::new(base_fire_target, config.currency);

    let post_fire_cashflow = match cashflow_mix {
        Some(mix) => Some(simulate_post_fire_cashflow(
            target_money,
            effective_monthly,
            mix,
        )?),
        None => None,
    };

    let sor_stress = match sor_config {
        Some(sor) => Some(run_sor_stress(config, target_money, sor)?),
        None => None,
    };

    Ok(FireSimulationReport {
        effective_monthly_expense: Money::new(
            round_money(effective_monthly.amount),
            config.currency,
        ),
        effective_annual_expense: effective_annual,
        base_fire_target_today: target_money,
        swr_pct,
        effective_return_pct: round_pct(net_return_pct),
        crossover: FireCrossoverMilestone {
            years_to_fire,
            months_to_fire,
            fire_age,
            fire_corpus_at_achieve,
            target_nominal_at_achieve,
        },
        points,
        archetypes,
        post_fire_cashflow,
        sor_stress,
    })
}

/// Calculates present corpus required today so compounding at the real return spread reaches
/// `target_corpus_at_retirement` without additional savings over `years_to_grow` years.
pub fn calculate_coast_fire_corpus(
    target_corpus_at_retirement: Money,
    years_to_grow: u16,
    expected_return_pct: Decimal,
    expected_inflation_pct: Decimal,
) -> Result<Money, FireError> {
    if target_corpus_at_retirement.amount < Decimal::ZERO {
        return Err(FireError::InvalidInput(
            "target_corpus_at_retirement cannot be negative".into(),
        ));
    }
    if years_to_grow == 0 {
        return Ok(Money::new(
            round_money(target_corpus_at_retirement.amount),
            target_corpus_at_retirement.currency,
        ));
    }
    let real_rate = (expected_return_pct - expected_inflation_pct) / Decimal::from(HUNDRED);
    if real_rate <= Decimal::ZERO {
        return Ok(Money::new(
            round_money(target_corpus_at_retirement.amount),
            target_corpus_at_retirement.currency,
        ));
    }
    let growth = (Decimal::ONE + real_rate)
        .checked_powi(i64::from(years_to_grow))
        .ok_or(FireError::Overflow)?;
    Ok(Money::new(
        round_money(target_corpus_at_retirement.amount / growth),
        target_corpus_at_retirement.currency,
    ))
}

/// Calculates the 5 FIRE archetypes (Lean, Standard, Fat, Coast, Barista).
pub fn calculate_fire_archetypes(
    config: &FirePlanConfig,
) -> Result<Vec<FireArchetypeCard>, FireError> {
    config.validate()?;
    let net_return_pct = effective_net_annual_return_pct(config);
    calculate_fire_archetypes_with_net(config, net_return_pct)
}

fn calculate_fire_archetypes_with_net(
    config: &FirePlanConfig,
    net_return_pct: Decimal,
) -> Result<Vec<FireArchetypeCard>, FireError> {
    let monthly = config.effective_monthly_expense()?.amount;
    let cur = config.current_corpus.amount;
    let hundred = Decimal::from(HUNDRED);
    let twelve = Decimal::from(TWELVE);

    let make_card = |id: FireArchetypeId,
                     multiplier: Decimal,
                     monthly_budget: Decimal,
                     target_val: Decimal,
                     coast_years: Option<u16>|
     -> FireArchetypeCard {
        let target_rounded = round_money(target_val);
        let shortfall = (target_rounded - cur).max(Decimal::ZERO);
        let progress = if target_rounded <= Decimal::ZERO {
            hundred
        } else {
            round_pct(((cur / target_rounded) * hundred).min(hundred))
        };
        FireArchetypeCard {
            id,
            multiplier,
            swr_pct: round_pct(hundred / multiplier),
            monthly_budget: Money::new(round_money(monthly_budget), config.currency),
            target_corpus: Money::new(target_rounded, config.currency),
            shortfall: Money::new(shortfall, config.currency),
            progress_pct: progress,
            coast_years_remaining: coast_years,
            achieved_today: cur >= target_rounded,
        }
    };

    // 1. Lean FIRE (0.75x expenses @ 22x)
    let lean_mult = Decimal::from(22);
    let lean_monthly = round_money(monthly * Decimal::new(75, 2));
    let lean_target = lean_monthly * twelve * lean_mult;

    // 2. Standard FIRE (1.0x @ config multiplier)
    let std_monthly = round_money(monthly);
    let std_target = std_monthly * twelve * config.multiplier;

    // 3. Fat FIRE (1.35x expenses @ 40x)
    let fat_mult = Decimal::from(40);
    let fat_monthly = round_money(monthly * Decimal::new(135, 2));
    let fat_target = fat_monthly * twelve * fat_mult;

    // 4. Coast FIRE (discounted Standard target by age max(current + 5, coast_target_age))
    let target_retire_age = config.coast_target_age.max(config.current_age + 5);
    let coast_years = target_retire_age - config.current_age;
    let coast_corpus = calculate_coast_fire_corpus(
        Money::new(std_target, config.currency),
        coast_years,
        net_return_pct,
        config.expected_inflation_pct,
    )?
    .amount;

    // 5. Barista FIRE (40% active consulting income -> 60% covered by portfolio)
    let barista_active = round_money(monthly * Decimal::new(40, 2));
    let barista_portfolio_monthly = monthly - barista_active;
    let barista_target = barista_portfolio_monthly * twelve * config.multiplier;

    Ok(vec![
        make_card(
            FireArchetypeId::Lean,
            lean_mult,
            lean_monthly,
            lean_target,
            None,
        ),
        make_card(
            FireArchetypeId::Standard,
            config.multiplier,
            std_monthly,
            std_target,
            None,
        ),
        make_card(
            FireArchetypeId::Fat,
            fat_mult,
            fat_monthly,
            fat_target,
            None,
        ),
        make_card(
            FireArchetypeId::Coast,
            config.multiplier,
            std_monthly,
            coast_corpus,
            Some(coast_years),
        ),
        make_card(
            FireArchetypeId::Barista,
            config.multiplier,
            barista_portfolio_monthly,
            barista_target,
            None,
        ),
    ])
}

/// Computes post-FIRE monthly passive income generation across the 3-bucket paycheck model.
pub fn simulate_post_fire_cashflow(
    corpus: Money,
    monthly_expense: Money,
    mix: &CashflowBucketConfig,
) -> Result<PostFireCashflowReport, FireError> {
    if corpus.currency != monthly_expense.currency {
        return Err(FireError::CurrencyMismatch {
            expected: corpus.currency,
            found: monthly_expense.currency,
        });
    }
    if corpus.amount < Decimal::ZERO {
        return Err(FireError::InvalidInput("corpus cannot be negative".into()));
    }
    if monthly_expense.amount <= Decimal::ZERO {
        return Err(FireError::InvalidInput(
            "monthly_expense must be strictly positive".into(),
        ));
    }
    mix.validate()?;

    let hundred = Decimal::from(HUNDRED);
    let twelve = Decimal::from(TWELVE);
    let cur = corpus.currency;

    let equity_corpus = corpus.amount * mix.equity_pct / hundred;
    let debt_corpus = corpus.amount * mix.debt_pct / hundred;
    let theta_corpus = corpus.amount * mix.theta_margin_pct / hundred;

    let monthly_equity =
        round_money(equity_corpus * (mix.equity_annual_yield_pct / hundred) / twelve);
    let monthly_debt = round_money(debt_corpus * (mix.debt_annual_yield_pct / hundred) / twelve);
    let monthly_theta = round_money(theta_corpus * (mix.theta_monthly_yield_pct / hundred));

    let total_passive = monthly_equity + monthly_debt + monthly_theta;
    let surplus = total_passive - monthly_expense.amount;
    let coverage_pct = round_pct((total_passive / monthly_expense.amount) * hundred);

    let net_annual_yield_pct = if corpus.amount.is_zero() {
        Decimal::ZERO
    } else {
        round_pct(((total_passive * twelve) / corpus.amount) * hundred)
    };
    let drawdown_pct = if corpus.amount.is_zero() {
        hundred
    } else {
        let shortfall_annual = (monthly_expense.amount - total_passive).max(Decimal::ZERO) * twelve;
        round_pct((shortfall_annual / corpus.amount) * hundred)
    };

    Ok(PostFireCashflowReport {
        monthly_equity_income: Money::new(monthly_equity, cur),
        monthly_debt_income: Money::new(monthly_debt, cur),
        monthly_theta_income: Money::new(monthly_theta, cur),
        total_passive_monthly: Money::new(total_passive, cur),
        monthly_expense_target: Money::new(round_money(monthly_expense.amount), cur),
        surplus_or_deficit: Money::new(round_money(surplus), cur),
        expense_coverage_pct: coverage_pct,
        net_annual_yield_pct,
        annual_principal_drawdown_pct: drawdown_pct,
    })
}

/// Runs a deterministic seeded Sequence-of-Returns Risk (SoRR) Monte Carlo / bootstrap
/// stress simulation over the post-FIRE withdrawal horizon in real ("today's money") terms.
pub fn run_sor_stress(
    config: &FirePlanConfig,
    initial_corpus: Money,
    sor: &SorStressConfig,
) -> Result<SorStressResult, FireError> {
    config.validate()?;
    sor.validate()?;
    if initial_corpus.currency != config.currency {
        return Err(FireError::CurrencyMismatch {
            expected: config.currency,
            found: initial_corpus.currency,
        });
    }
    if initial_corpus.amount < Decimal::ZERO {
        return Err(FireError::InvalidInput(
            "initial_corpus cannot be negative".into(),
        ));
    }

    let hundred = Decimal::from(HUNDRED);
    let annual_expense_real = config.effective_monthly_expense()?.amount * Decimal::from(TWELVE);
    let net_return_pct = effective_net_annual_return_pct(config);
    let mean_nominal = net_return_pct / hundred;
    let inflation_frac = config.expected_inflation_pct / hundred;
    let vol_frac = sor.annual_volatility_pct / hundred;
    let mean_real =
        ((Decimal::ONE + mean_nominal) / (Decimal::ONE + inflation_frac)) - Decimal::ONE;

    let mut rng = SplitMix64::new(sor.seed);
    let mut terminals: Vec<Decimal> = Vec::with_capacity(sor.iterations as usize);
    let mut survivors: u32 = 0;
    let mut worst_depletion: Option<u16> = None;

    for _ in 0..sor.iterations {
        let mut wealth = initial_corpus.amount;
        let mut depleted = false;

        for yr in 1..=sor.post_fire_years {
            let annual_real_return = match &sor.historical_annual_returns {
                Some(hist) => {
                    let idx = rng.next_index(hist.len());
                    let r_nom = hist[idx];
                    ((Decimal::ONE + r_nom) / (Decimal::ONE + inflation_frac)) - Decimal::ONE
                }
                None => {
                    // Draw index in 0..256: high bit selects sign, low 7 bits select symmetric N(0, 1) quantile.
                    let draw = rng.next_index(256);
                    let mag = Decimal::new(NORMAL_POS_QUANTILES_1E8[draw & 0x7F], 8);
                    let z = if draw >= 128 { mag } else { -mag };
                    (mean_real + vol_frac * z).max(Decimal::new(-95, 2))
                }
            };

            // Withdraw annual expenses linearly across the year on average (mid-year approximation).
            wealth = (wealth - annual_expense_real) * (Decimal::ONE + annual_real_return);
            if wealth <= Decimal::ZERO {
                wealth = Decimal::ZERO;
                depleted = true;
                worst_depletion = Some(match worst_depletion {
                    Some(prev) => prev.min(yr),
                    None => yr,
                });
                break;
            }
        }

        if !depleted {
            survivors += 1;
        }
        terminals.push(round_money(wealth));
    }

    terminals.sort();
    let n = terminals.len();
    let idx_p10 = ((n * 10) / 100).min(n - 1);
    let idx_p50 = ((n * 50) / 100).min(n - 1);
    let idx_p90 = ((n * 90) / 100).min(n - 1);

    let survival_pct =
        round_pct((Decimal::from(survivors) / Decimal::from(sor.iterations)) * hundred);

    Ok(SorStressResult {
        survival_probability_pct: survival_pct,
        p10_terminal_real: Money::new(terminals[idx_p10], config.currency),
        p50_terminal_real: Money::new(terminals[idx_p50], config.currency),
        p90_terminal_real: Money::new(terminals[idx_p90], config.currency),
        worst_depletion_year: worst_depletion,
    })
}
