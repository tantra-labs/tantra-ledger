//! Deterministic per-position trade state machine (Plan 0005, slice P2).
//!
//! [`TradeManager`] runs a completed [`TradePlan`] bar by bar: entry-fill
//! re-anchoring, breakeven moves, monotone trailing stops, target-ladder
//! scale-outs, stop exits, and session-close rules (`CloseMa`,
//! `AfterSessions`, `TimeStop`). It is the single engine behind backtest,
//! the Python FFI, and the live runtime.
//!
//! Zero-I/O, `Decimal`-only, WASM-clean: every timestamp is injected, no wall
//! clock, no UUID minting, no I/O. All state is plain serde so a position can
//! be persisted and resumed mid-flight.
//!
//! ## Intrabar policy
//!
//! [`TradeManager::on_bar`] is conservative: when both the stop and a target
//! sit inside a bar's high/low, the stop wins (stop-first). The stop is
//! monotone — trailing and breakeven moves may only tighten it, never loosen.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use tantra_core::{
    Bar, BreakevenTrigger, InstrumentId, MaKind, OrderSide, PlanError, PlannedTarget, Price,
    Quantity, TimeStop, Timeframe, TradePlan, TrailRule, TrailSpec,
};

/// One action the trade manager wants executed against the position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TradeAction {
    /// Move the working stop to `new` (always a tightening move).
    MoveStop {
        /// The new stop price.
        new: Price,
    },
    /// Exit `qty` at the market (a scale-out leg).
    PartialExit {
        /// Quantity to exit.
        qty: Quantity,
        /// Human-readable reason (`"target at 2R"`, `"time exit after 3 session(s)"`, ...).
        reason: String,
    },
    /// Close the entire remaining position.
    ExitAll {
        /// Human-readable reason (`"stop"`, `"close_ma"`, `"time_stop"`, ...).
        reason: String,
    },
}

/// Injected per-bar market/snapshot inputs for the trade manager.
///
/// Deliberately minimal and `Decimal`-only; the feed (not the manager) owns
/// the indicators. All values are injected — zero I/O, zero wall clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TradeSnapshot {
    /// Latest traded price (used for R-multiple / activation math).
    pub last_price: Price,
    /// ATR of the bar interval, when computed (trail moves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atr: Option<Decimal>,
    /// Moving-average values `(kind, period, value)` consumed by close-based
    /// trailing exits.
    #[serde(default)]
    pub ma_values: Vec<(MaKind, u32, Decimal)>,
    /// Session high (day-extreme / Chandelier trail updates), when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<Price>,
    /// Session low (day-extreme / Chandelier trail updates), when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<Price>,
    /// When the snapshot was taken (injected).
    pub timestamp: DateTime<Utc>,
}

/// Kind of a remaining target leg inside the manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActiveTargetKind {
    /// An R-multiple leg; its price re-prices off the actual fill.
    R,
    /// An absolute price leg (price target / measured move); the level stays
    /// as planned.
    Absolute,
    /// A session-count leg; it fires after `N` sessions at session close.
    AfterSessions,
}

/// One remaining scale-out leg inside the [`TradeManager`] state.
///
/// Part of the persisted position state; the R multiple keeps the leg
/// re-priceable off the actual fill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActiveTarget {
    /// Kind of trigger this leg carries.
    pub kind: ActiveTargetKind,
    /// R multiple from the plan entry (signed away from the stop; `0` for
    /// absolute-price legs).
    pub r: Decimal,
    /// Current price level (re-priced off the fill for R-multiple legs).
    pub price: Price,
    /// Quantity remaining to exit at this leg.
    pub qty: Quantity,
    /// Human-readable reason (`"target at 2R"`, `"measured move"`, ...).
    pub reason: String,
    /// Session count for `AfterSessions` legs.
    pub sessions: u32,
}

/// Journal entry recorded by the [`TradeManager`] (persisted, injected ts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TradeManagerEvent {
    /// The position was opened.
    EntryFilled {
        /// Injected fill timestamp.
        ts: DateTime<Utc>,
        /// Actual fill price.
        fill_price: Price,
        /// Filled quantity.
        qty: Quantity,
    },
    /// The working stop moved (always a tightening move).
    StopMoved {
        /// Injected timestamp.
        ts: DateTime<Utc>,
        /// Previous stop price.
        from: Price,
        /// New stop price.
        to: Price,
    },
    /// A scale-out leg filled.
    PartialExit {
        /// Injected timestamp.
        ts: DateTime<Utc>,
        /// Quantity exited.
        qty: Quantity,
        /// Fill price.
        price: Price,
        /// Reason (`"target at 2R"`, `"time exit after 3 session(s)"`, ...).
        reason: String,
    },
    /// The position was closed.
    ExitAll {
        /// Injected timestamp.
        ts: DateTime<Utc>,
        /// Quantity closed.
        qty: Quantity,
        /// Fill price.
        price: Price,
        /// Reason (`"stop"`, `"close_ma"`, `"time_stop"`, ...).
        reason: String,
    },
}

/// A session-close exit scheduled to execute at the next bar's open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum PendingExit {
    /// Close the whole position.
    All {
        /// Reason for the exit.
        reason: String,
    },
    /// Exit a fixed quantity.
    Partial {
        /// Quantity to exit.
        qty: Quantity,
        /// Reason for the exit.
        reason: String,
    },
}

/// Deterministic, per-position state machine driven by a completed [`TradePlan`].
///
/// State is plain serde (`Serialize + Deserialize + JsonSchema`) so a live
/// position can be persisted and resumed. Construction is cheap; the position
/// only becomes real once [`TradeManager::on_entry_fill`] succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TradeManager {
    /// The instrument being traded.
    instrument: InstrumentId,
    /// Trade side.
    side: OrderSide,
    /// The plan's conservative entry reference.
    entry_reference: Price,
    /// Actual fill price, once filled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fill_price: Option<Price>,
    /// The plan's initial stop (never moves).
    initial_stop: Price,
    /// The current working stop (monotone — only tightens).
    stop: Price,
    /// Per-unit risk: `|fill - initial_stop|` once re-anchored.
    risk_per_unit: Decimal,
    /// Whether the entry fill has been applied.
    filled: bool,
    /// Quantity planned by the sizing step.
    planned_quantity: Quantity,
    /// Remaining open quantity (never negative).
    open_quantity: Quantity,
    /// Remaining target ladder, front-first.
    targets: Vec<ActiveTarget>,
    /// Trailing-stop configuration.
    trail: TrailSpec,
    /// Optional breakeven trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    breakeven_trigger: Option<BreakevenTrigger>,
    /// The breakeven level (the actual fill price).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    breakeven_level: Option<Price>,
    /// Whether the stop has already been moved to breakeven.
    stop_at_breakeven: bool,
    /// Optional time stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    time_stop: Option<TimeStop>,
    /// Fill-time minimum reward/risk guard (the plan's committed reward/risk).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min_reward_risk: Option<Decimal>,
    /// Sum of closed exits, exact `Decimal`.
    realized_pnl: Decimal,
    /// Journal of every action the manager took.
    events: Vec<TradeManagerEvent>,
    /// Running extreme high (trail anchors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extreme_high: Option<Price>,
    /// Running extreme low (trail anchors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extreme_low: Option<Price>,
    /// Bars seen per timeframe (time-stop and session triggers).
    bars_per_timeframe: Vec<(Timeframe, u32)>,
    /// Sessions the position has been open through.
    sessions_open: u32,
    /// Session-close exits scheduled to execute at the next bar's open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_exits: Vec<PendingExit>,
    /// Whether the first target leg has filled (breakeven trigger).
    first_target_filled: bool,
}

impl TradeManager {
    /// Build the state machine from a completed plan.
    ///
    /// Carries the instrument, side, entry reference, initial stop, per-unit
    /// risk, planned quantity, the remaining target ladder, the trail spec,
    /// the breakeven trigger, and the time stop. The position is not yet
    /// "filled"; call [`TradeManager::on_entry_fill`] first.
    ///
    /// # Errors
    /// Returns [`PlanError::ZeroRisk`] when the plan's risk distance is zero.
    pub fn new(plan: &TradePlan) -> Result<Self, PlanError> {
        if plan.risk_per_unit <= Decimal::ZERO {
            return Err(PlanError::ZeroRisk);
        }
        let targets = plan
            .targets
            .iter()
            .map(|target| ActiveTarget::from_plan(target, plan))
            .collect();
        Ok(Self {
            instrument: plan.instrument.clone(),
            side: plan.side,
            entry_reference: plan.entry,
            fill_price: None,
            initial_stop: plan.initial_stop,
            stop: plan.initial_stop,
            risk_per_unit: plan.risk_per_unit,
            filled: false,
            planned_quantity: plan.quantity,
            open_quantity: plan.quantity,
            targets,
            trail: plan.trail.clone(),
            breakeven_trigger: plan.trail.breakeven_after,
            breakeven_level: None,
            stop_at_breakeven: false,
            time_stop: plan.trail.time_stop,
            min_reward_risk: Some(plan.reward_risk),
            realized_pnl: Decimal::ZERO,
            events: Vec::new(),
            extreme_high: None,
            extreme_low: None,
            bars_per_timeframe: Vec::new(),
            sessions_open: 0,
            pending_exits: Vec::new(),
            first_target_filled: false,
        })
    }

    /// Apply the actual entry fill and re-anchor the whole plan off it.
    ///
    /// Re-prices `risk_per_unit = |fill_price - initial_stop|`, the R-multiple
    /// target ladder, and the breakeven level off the actual fill, then opens
    /// the position. Fails closed — no partial state — when the gap-fill
    /// collapses risk (fill on the wrong side of the stop →
    /// [`PlanError::ZeroRisk`]) or the re-priced first target falls below the
    /// committed reward/risk ([`PlanError::InsufficientReward`]).
    ///
    /// # Errors
    /// [`PlanError::ZeroRisk`] / [`PlanError::InsufficientReward`] as above;
    /// [`PlanError::MissingInput`] on a double fill.
    pub fn on_entry_fill(
        &mut self,
        fill_price: Price,
        fill_qty: Quantity,
        ts: DateTime<Utc>,
    ) -> Result<Vec<TradeAction>, PlanError> {
        if self.filled {
            return Err(PlanError::MissingInput("double fill"));
        }
        let fill = fill_price.value();
        let stop = self.initial_stop.value();
        let risk_ok = match self.side {
            OrderSide::Buy => fill > stop,
            OrderSide::Sell => fill < stop,
        };
        if !risk_ok {
            return Err(PlanError::ZeroRisk);
        }
        let new_risk = (fill - stop).abs();
        if new_risk == Decimal::ZERO {
            return Err(PlanError::ZeroRisk);
        }

        // Re-price the R-multiple ladder off the actual fill. All validation
        // happens before any state mutation (never a partial plan).
        let mut re_priced = Vec::with_capacity(self.targets.len());
        for target in &self.targets {
            let mut updated = target.clone();
            if target.kind == ActiveTargetKind::R {
                updated.price = Price::try_new(
                    match self.side {
                        OrderSide::Buy => fill + target.r * new_risk,
                        OrderSide::Sell => fill - target.r * new_risk,
                    }
                    .max(Decimal::ZERO),
                )
                .expect("non-negative price");
            }
            re_priced.push(updated);
        }

        // Fill-time reward/risk guard.
        if let Some(min_rr) = self.min_reward_risk {
            let fill_rr = re_priced.first().map_or(Decimal::ZERO, |target| target.r);
            if fill_rr < min_rr {
                return Err(PlanError::InsufficientReward);
            }
        }

        self.filled = true;
        self.fill_price = Some(fill_price);
        self.open_quantity = fill_qty;
        self.risk_per_unit = new_risk;
        self.targets = re_priced;
        self.breakeven_level = Some(fill_price);
        self.events.push(TradeManagerEvent::EntryFilled {
            ts,
            fill_price,
            qty: fill_qty,
        });
        Ok(Vec::new())
    }

    /// Process one bar: intrabar high/low price triggers.
    ///
    /// Order is deterministic and conservative:
    /// 1. pending session-close exits execute at this bar's open (no look-ahead),
    /// 2. running extremes update from the bar and snapshot,
    /// 3. the trailing stop tightens monotonically once activated,
    /// 4. the breakeven trigger fires,
    /// 5. the stop is checked first (stop-first policy: when both the stop and
    ///    a target sit inside the bar's high/low, the stop wins),
    /// 6. target-ladder legs fill front-first.
    ///
    /// No action ever exits more than the open quantity (short-flip guard).
    pub fn on_bar(&mut self, bar: &Bar, snapshot: &TradeSnapshot) -> Vec<TradeAction> {
        if !self.filled || self.is_closed() {
            return Vec::new();
        }
        let mut actions = Vec::new();
        let ts = bar.timestamp;

        // 1. Pending session-close exits execute at the open of this bar.
        for pending in std::mem::take(&mut self.pending_exits) {
            match pending {
                PendingExit::All { reason } => {
                    let action = TradeAction::ExitAll {
                        reason: reason.clone(),
                    };
                    self.close_position(bar.open, reason, ts);
                    actions.push(action);
                    return actions;
                }
                PendingExit::Partial { qty, reason } => {
                    let qty = qty.min(self.open_quantity);
                    if qty > Quantity::ZERO {
                        self.realized_pnl += self.pnl_for(bar.open, qty);
                        self.open_quantity =
                            Quantity::try_new(self.open_quantity.value() - qty.value())
                                .expect("non-negative quantity");
                        self.events.push(TradeManagerEvent::PartialExit {
                            ts,
                            qty,
                            price: bar.open,
                            reason: reason.clone(),
                        });
                        actions.push(TradeAction::PartialExit { qty, reason });
                    }
                }
            }
        }
        if self.is_closed() {
            return actions;
        }

        // 2. Running extremes.
        let bar_high = bar.high.value();
        let bar_low = bar.low.value();
        let snap_high = snapshot.high.map_or(bar_high, |price| price.value());
        let snap_low = snapshot.low.map_or(bar_low, |price| price.value());
        let high = Price::try_new(bar_high.max(snap_high)).expect("non-negative price");
        let low = Price::try_new(bar_low.min(snap_low)).expect("non-negative price");
        self.extreme_high = Some(match self.extreme_high {
            Some(existing) => existing.max(high),
            None => high,
        });
        self.extreme_low = Some(match self.extreme_low {
            Some(existing) => existing.min(low),
            None => low,
        });

        // 3. Trailing stop (monotone) once activated.
        if self.r_at(snapshot.last_price) >= self.trail.activation_after_r
            && let Some(candidate) = self.trail_candidate(snapshot)
            && let Some(action) = self.tighten_stop(candidate, ts)
        {
            actions.push(action);
        }

        // 4. Breakeven trigger.
        if !self.stop_at_breakeven {
            let fires = match self.breakeven_trigger {
                Some(BreakevenTrigger::AtR(r)) => self.r_at(snapshot.last_price) >= r,
                Some(BreakevenTrigger::AfterSessions(n)) => self.sessions_open >= n,
                Some(BreakevenTrigger::AfterFirstTarget) => self.first_target_filled,
                None => false,
            };
            if fires {
                if let Some(level) = self.breakeven_level
                    && let Some(action) = self.tighten_stop(level, ts)
                {
                    actions.push(action);
                }
                self.stop_at_breakeven = true;
            }
        }

        // 5. Stop-first: when the stop is inside the bar, the stop wins.
        let stop_hit = match self.side {
            OrderSide::Buy => bar.low.value() <= self.stop.value(),
            OrderSide::Sell => bar.high.value() >= self.stop.value(),
        };
        if stop_hit {
            let reason = "stop".to_string();
            let action = TradeAction::ExitAll {
                reason: reason.clone(),
            };
            self.close_position(self.stop, reason, ts);
            actions.push(action);
            return actions;
        }

        // 6. Target-ladder fills, front-first (FIFO).
        let mut i = 0;
        while i < self.targets.len() {
            let touched = match self.side {
                OrderSide::Buy => bar.high.value() >= self.targets[i].price.value(),
                OrderSide::Sell => bar.low.value() <= self.targets[i].price.value(),
            };
            if touched {
                let leg = self.targets.remove(i);
                let qty = leg.qty.min(self.open_quantity);
                if qty > Quantity::ZERO {
                    self.realized_pnl += self.pnl_for(leg.price, qty);
                    self.open_quantity =
                        Quantity::try_new(self.open_quantity.value() - qty.value())
                            .expect("non-negative quantity");
                    self.events.push(TradeManagerEvent::PartialExit {
                        ts,
                        qty,
                        price: leg.price,
                        reason: leg.reason.clone(),
                    });
                    actions.push(TradeAction::PartialExit {
                        qty,
                        reason: leg.reason.clone(),
                    });
                    self.first_target_filled = true;
                }
            } else {
                i += 1;
            }
        }
        actions
    }

    /// Evaluate close-only rules at the session close.
    ///
    /// Rules evaluated here execute on the **next** bar/session (no
    /// look-ahead): [`TrailRule::CloseMa`] (long exits on a close below the
    /// matching MA value, short on a close above),
    /// [`BreakevenTrigger::AfterSessions`], [`TargetTrigger::AfterSessions`]
    /// legs, and the [`TimeStop`]. Exits are scheduled as pending and filled at
    /// the next bar's open.
    ///
    /// Bars are counted per timeframe for the time stop and session triggers.
    pub fn on_session_close(
        &mut self,
        timeframe: Timeframe,
        bar: &Bar,
        snapshot: &TradeSnapshot,
    ) -> Vec<TradeAction> {
        if !self.filled || self.is_closed() || !self.pending_exits.is_empty() {
            return Vec::new();
        }
        let mut actions = Vec::new();
        let ts = bar.timestamp;

        // Bar / session counters.
        self.increment_bars(timeframe);
        self.sessions_open += 1;

        let close = bar.close.value();
        let r_now = self.r_at(bar.close);

        // CloseMa trailing exit.
        if r_now >= self.trail.activation_after_r
            && let TrailRule::CloseMa { kind, period } = &self.trail.rule
        {
            let crossed = snapshot
                .ma_values
                .iter()
                .find(|(k, p, _)| k == kind && *p == *period)
                .map(|(_, _, value)| match self.side {
                    OrderSide::Buy => close < *value,
                    OrderSide::Sell => close > *value,
                })
                .unwrap_or(false);
            if crossed {
                let reason = "close_ma".to_string();
                actions.push(TradeAction::ExitAll {
                    reason: reason.clone(),
                });
                self.pending_exits.push(PendingExit::All { reason });
                return actions;
            }
        }

        // Breakeven after N sessions: the stop moves to the fill at the close
        // of the Nth session.
        if !self.stop_at_breakeven {
            let fires = matches!(
                self.breakeven_trigger,
                Some(BreakevenTrigger::AfterSessions(n)) if self.sessions_open >= n
            );
            if fires {
                if let Some(level) = self.breakeven_level
                    && let Some(action) = self.tighten_stop(level, ts)
                {
                    actions.push(action);
                }
                self.stop_at_breakeven = true;
            }
        }

        // AfterSessions target legs.
        let mut i = 0;
        while i < self.targets.len() {
            let fires = self.targets[i].kind == ActiveTargetKind::AfterSessions
                && self.sessions_open >= self.targets[i].sessions;
            if fires {
                let leg = self.targets.remove(i);
                actions.push(TradeAction::PartialExit {
                    qty: leg.qty,
                    reason: leg.reason.clone(),
                });
                self.pending_exits.push(PendingExit::Partial {
                    qty: leg.qty,
                    reason: leg.reason,
                });
            } else {
                i += 1;
            }
        }

        // Time stop.
        if let Some(time_stop) = self.time_stop
            && time_stop.timeframe == timeframe
        {
            let bars = self.bar_count(timeframe);
            if bars >= time_stop.bars && r_now < time_stop.min_progress_r {
                let reason = "time_stop".to_string();
                actions.push(TradeAction::ExitAll {
                    reason: reason.clone(),
                });
                self.pending_exits.push(PendingExit::All { reason });
            }
        }

        actions
    }

    /// Remaining open quantity.
    #[must_use]
    pub fn open_quantity(&self) -> Quantity {
        self.open_quantity
    }

    /// Whether the position is closed (filled and flat).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.filled && self.open_quantity == Quantity::ZERO
    }

    /// Sum of closed exits, exact `Decimal` (losses are negative).
    #[must_use]
    pub fn realized_pnl(&self) -> Decimal {
        self.realized_pnl
    }

    /// The manager's action journal.
    #[must_use]
    pub fn events(&self) -> &[TradeManagerEvent] {
        &self.events
    }

    /// The current working stop.
    #[must_use]
    pub fn stop(&self) -> Price {
        self.stop
    }

    /// The actual fill price, once filled.
    #[must_use]
    pub fn fill_price(&self) -> Option<Price> {
        self.fill_price
    }

    /// The re-anchored per-unit risk.
    #[must_use]
    pub fn risk_per_unit(&self) -> Decimal {
        self.risk_per_unit
    }

    /// The trade side.
    #[must_use]
    pub const fn side(&self) -> OrderSide {
        self.side
    }

    /// The instrument being traded.
    #[must_use]
    pub fn instrument(&self) -> &InstrumentId {
        &self.instrument
    }

    /// The remaining target ladder, front-first.
    #[must_use]
    pub fn remaining_targets(&self) -> &[ActiveTarget] {
        &self.targets
    }

    /// Close the entire remaining position at `price`.
    fn close_position(&mut self, price: Price, reason: String, ts: DateTime<Utc>) {
        let qty = self.open_quantity;
        if qty > Quantity::ZERO {
            self.realized_pnl += self.pnl_for(price, qty);
            self.open_quantity = Quantity::ZERO;
            self.events.push(TradeManagerEvent::ExitAll {
                ts,
                qty,
                price,
                reason,
            });
        }
    }

    /// Tighten the stop to `new` (monotone: longs never lower, shorts never
    /// raise). Emits a [`TradeAction::MoveStop`] only on a strict move.
    fn tighten_stop(&mut self, new: Price, ts: DateTime<Utc>) -> Option<TradeAction> {
        let tightens = match self.side {
            OrderSide::Buy => new.value() > self.stop.value(),
            OrderSide::Sell => new.value() < self.stop.value(),
        };
        if tightens {
            let from = self.stop;
            self.stop = new;
            self.events
                .push(TradeManagerEvent::StopMoved { ts, from, to: new });
            Some(TradeAction::MoveStop { new })
        } else {
            None
        }
    }

    /// Candidate trailing stop from the active trail rule (`None` when the
    /// rule has no intrabar price trigger).
    fn trail_candidate(&self, snapshot: &TradeSnapshot) -> Option<Price> {
        match &self.trail.rule {
            TrailRule::Off | TrailRule::CloseMa { .. } => None,
            TrailRule::Atr { mult } | TrailRule::Chandelier { mult, .. } => {
                let atr = snapshot.atr?;
                let extreme = match self.side {
                    OrderSide::Buy => self.extreme_high?,
                    OrderSide::Sell => self.extreme_low?,
                };
                Some(match self.side {
                    OrderSide::Buy => {
                        Price::try_new((extreme.value() - *mult * atr).max(Decimal::ZERO))
                            .expect("non-negative price")
                    }
                    OrderSide::Sell => {
                        Price::try_new((extreme.value() + *mult * atr).max(Decimal::ZERO))
                            .expect("non-negative price")
                    }
                })
            }
            TrailRule::Swing => match self.side {
                OrderSide::Buy => snapshot.low.or(self.extreme_low),
                OrderSide::Sell => snapshot.high.or(self.extreme_high),
            },
            TrailRule::Percent(pct) => {
                let extreme = match self.side {
                    OrderSide::Buy => self.extreme_high?,
                    OrderSide::Sell => self.extreme_low?,
                };
                Some(match self.side {
                    OrderSide::Buy => {
                        Price::try_new((extreme.value() * (Decimal::ONE - *pct)).max(Decimal::ZERO))
                            .expect("non-negative price")
                    }
                    OrderSide::Sell => {
                        Price::try_new((extreme.value() * (Decimal::ONE + *pct)).max(Decimal::ZERO))
                            .expect("non-negative price")
                    }
                })
            }
        }
    }

    /// Current profit in R multiples (against the fill).
    fn r_at(&self, price: Price) -> Decimal {
        let Some(fill) = self.fill_price else {
            return Decimal::ZERO;
        };
        if self.risk_per_unit == Decimal::ZERO {
            return Decimal::ZERO;
        }
        match self.side {
            OrderSide::Buy => (price.value() - fill.value()) / self.risk_per_unit,
            OrderSide::Sell => (fill.value() - price.value()) / self.risk_per_unit,
        }
    }

    /// Signed PnL of exiting `qty` at `price`.
    fn pnl_for(&self, price: Price, qty: Quantity) -> Decimal {
        let Some(fill) = self.fill_price else {
            return Decimal::ZERO;
        };
        match self.side {
            OrderSide::Buy => (price.value() - fill.value()) * qty.value(),
            OrderSide::Sell => (fill.value() - price.value()) * qty.value(),
        }
    }

    fn bar_count(&self, timeframe: Timeframe) -> u32 {
        self.bars_per_timeframe
            .iter()
            .find(|(tf, _)| *tf == timeframe)
            .map_or(0, |(_, count)| *count)
    }

    fn increment_bars(&mut self, timeframe: Timeframe) {
        if let Some((_, count)) = self
            .bars_per_timeframe
            .iter_mut()
            .find(|(tf, _)| *tf == timeframe)
        {
            *count += 1;
        } else {
            self.bars_per_timeframe.push((timeframe, 1));
        }
    }
}

impl ActiveTarget {
    /// Recover a manager leg from a planned target: classify the trigger kind
    /// from the planner's canonical reason string and derive the R multiple
    /// from the plan geometry.
    fn from_plan(target: &PlannedTarget, plan: &TradePlan) -> Self {
        let entry = plan.entry.value();
        let risk = plan.risk_per_unit;
        let r = if risk == Decimal::ZERO {
            Decimal::ZERO
        } else {
            match plan.side {
                OrderSide::Buy => (target.price.value() - entry) / risk,
                OrderSide::Sell => (entry - target.price.value()) / risk,
            }
        };
        Self {
            kind: classify_reason(&target.reason),
            r,
            price: target.price,
            qty: target.qty,
            reason: target.reason.clone(),
            sessions: session_count(&target.reason),
        }
    }
}

/// Classify a target leg from the planner's canonical reason string.
///
/// `TradePlanner` emits exactly `"target at {r}R"`, `"price target"`,
/// `"measured move"`, and `"time exit after {n} session(s)"`.
fn classify_reason(reason: &str) -> ActiveTargetKind {
    if reason.starts_with("time exit after ") {
        ActiveTargetKind::AfterSessions
    } else if reason == "price target" || reason == "measured move" {
        ActiveTargetKind::Absolute
    } else {
        ActiveTargetKind::R
    }
}

/// Parse the session count out of `"time exit after {n} session(s)"`.
fn session_count(reason: &str) -> u32 {
    reason
        .strip_prefix("time exit after ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|count| count.parse().ok())
        .unwrap_or(0)
}
