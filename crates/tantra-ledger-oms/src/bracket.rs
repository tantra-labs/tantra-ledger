//! # Bracket-order (SL / multi-leg target / TSL) OMS lifecycle
//!
//! Gap 0012 — a bracket is one N-leg domain aggregate owned by the OMS:
//!
//! ```text
//! BracketOrder = parent entry order
//!             + resting child stop-loss order (price + qty)
//!             + Vec<PlannedTarget> partial-exit target ladder
//!             + optional trailing rule
//! ```
//!
//! The bracket stays **open** while `open_qty > 0`: a partial exit (say ⅓ of the
//! position) shrinks the runner instead of closing the trade, so a 1-to-1
//! single-target model can never represent scale-outs correctly. Every mutation
//! — entry fill, stop price/qty modify, partial exit, stop trigger — is a
//! first-class [`BracketEvent`] journaled on the aggregate with an injected
//! `DateTime<Utc>` (no wall clock, no UUID minting inside the domain; the
//! caller-supplied `OrderRequest` / `FillReport` carry all identifiers).
//!
//! Invariants enforced here:
//!
//! - The initial stop must sit on the **protective** side of the reference entry
//!   price (buy: strictly below; sell: strictly above).
//! - A trailing-stop update may only **tighten** — a long stop moves up or holds
//!   (`new >= resting`), a short stop moves down or holds (`new <= resting`);
//!   any widening request is refused with
//!   [`OrderError::StopWidening`] (mirrors `tantra-risk` gap 0005,
//!   [`tantra_risk::trade_plan_gate::validate_stop_update`]).
//! - The resting stop quantity may never exceed the remaining open quantity
//!   (no short-flip guard).
//! - A partial exit can never push `open_qty` below zero.
//!
//! P&L realisation stays inside the bracket as bracket events (journal bracket
//! P&L as bracket events) rather than FIFO tax-lot realisation: `portfolio`
//! tax-lot accounting lives in `tantra-ledger-portfolio`, which this crate does
//! not depend on.
//!
//! [`tantra_risk::trade_plan_gate::validate_stop_update`]: ../../../tantra-risk/crates/tantra-risk/src/trade_plan_gate.rs

use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tantra_core::{FillReport, OrderRequest, OrderSide, PlannedTarget, Price, Quantity, TrailRule};
use uuid::Uuid;

use crate::OrderError;

// ———————————————————————————————————————————————————————————————————
// Identifiers, events, aggregate
// ———————————————————————————————————————————————————————————————————

/// Stable identifier of a [`BracketOrder`] aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct BracketId(pub String);

impl BracketId {
    /// Wrap a raw bracket identifier string.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the raw identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Deterministically derive a bracket ID from the entry order's client
    /// correlation UUID (no UUID minting inside the domain).
    #[must_use]
    pub fn from_client_order_id(client_order_id: Uuid) -> Self {
        Self(format!("BRK-{client_order_id}"))
    }
}

impl fmt::Display for BracketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One journaled lifecycle event of a [`BracketOrder`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BracketEvent {
    /// The parent entry order filled; the position is now open.
    EntryFilled {
        /// Execution price of the entry fill.
        price: Price,
        /// Quantity opened.
        qty: Quantity,
        /// Event timestamp (injected).
        ts: DateTime<Utc>,
    },
    /// The resting stop price and/or quantity changed.
    StopMoved {
        /// Previous resting stop price.
        old: Price,
        /// New resting stop price.
        new: Price,
        /// Resting stop quantity after the update.
        qty: Quantity,
        /// Event timestamp (injected).
        ts: DateTime<Utc>,
    },
    /// A partial-exit target leg filled; the bracket stays open for the runner.
    PartialExit {
        /// Execution price of the exit leg.
        price: Price,
        /// Quantity exited.
        qty: Quantity,
        /// Event timestamp (injected).
        ts: DateTime<Utc>,
    },
    /// The resting stop was triggered, closing the runner.
    StopTriggered {
        /// Price at which the stop triggered.
        price: Price,
        /// Quantity stopped out.
        qty: Quantity,
        /// Event timestamp (injected).
        ts: DateTime<Utc>,
    },
    /// The bracket reached its terminal closed state.
    Closed {
        /// Why the bracket closed (`stop_triggered`, `all_targets_exited`, ...).
        reason: String,
        /// Realized PnL closed out by this bracket (exact `Decimal`, in the
        /// entry's currency).
        realized_pnl: Decimal,
        /// Event timestamp (injected).
        ts: DateTime<Utc>,
    },
}

/// N-leg bracket aggregate: parent entry + resting stop + target ladder.
///
/// `open_qty` is the remaining runner quantity; the bracket closes only when
/// `open_qty` reaches zero (stop trigger, final target, or a full-exit close).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BracketOrder {
    /// Stable aggregate identifier (deterministic, derived from the entry).
    pub bracket_id: BracketId,
    /// The parent entry [`OrderRequest`].
    pub entry: OrderRequest,
    /// The entry fill once the position is open.
    pub entry_fill: Option<FillReport>,
    /// The stop as planned by the trade plan (immutable reference point).
    pub initial_stop: Price,
    /// The current resting stop price (trails but never widens).
    pub resting_stop: Price,
    /// The current resting stop quantity.
    pub resting_stop_qty: Quantity,
    /// The full planned target ladder.
    pub targets: Vec<PlannedTarget>,
    /// The not-yet-exited portion of the target ladder (FIFO).
    pub remaining_targets: Vec<PlannedTarget>,
    /// Remaining position after partial exits (the runner).
    pub open_qty: Quantity,
    /// The trailing-stop rule, when the plan carries one.
    pub trail: Option<TrailRule>,
    /// True once the bracket reached its terminal closed state.
    pub closed: bool,
    /// Append-only journal of every lifecycle event.
    pub events: Vec<BracketEvent>,
}

impl BracketOrder {
    /// Construct a bracket from a plan's entry order, initial stop and target
    /// ladder.
    ///
    /// The bracket ID is derived deterministically from the entry's client
    /// correlation UUID. The stop must sit on the protective side of the entry
    /// reference price: strictly below the entry limit price for a buy,
    /// strictly above for a sell. When the entry order carries no limit price
    /// (a market entry) the side check cannot be evaluated at construction time
    /// and is skipped — the caller is expected to have validated the stop
    /// against the market reference already.
    ///
    /// # Errors
    /// [`OrderError::InvalidBracket`] when the stop sits on the wrong side of
    /// the reference price or any target quantity is non-positive.
    pub fn new(
        entry: OrderRequest,
        initial_stop: Price,
        targets: Vec<PlannedTarget>,
        trail: Option<TrailRule>,
    ) -> Result<Self, OrderError> {
        if let Some(reference) = entry.order_type.limit_price() {
            match entry.side {
                OrderSide::Buy if initial_stop.value() >= reference.value() => {
                    return Err(OrderError::InvalidBracket(format!(
                        "initial stop {initial_stop} must be strictly below the entry reference {reference} for a buy"
                    )));
                }
                OrderSide::Sell if initial_stop.value() <= reference.value() => {
                    return Err(OrderError::InvalidBracket(format!(
                        "initial stop {initial_stop} must be strictly above the entry reference {reference} for a sell"
                    )));
                }
                _ => {}
            }
        }
        for target in &targets {
            if !target.qty.is_positive() {
                return Err(OrderError::InvalidBracket(format!(
                    "target '{}' has non-positive quantity {qty}",
                    target.reason,
                    qty = target.qty
                )));
            }
        }

        let bracket_id = BracketId::from_client_order_id(entry.client_order_id);
        let remaining_targets = targets.clone();
        Ok(Self {
            bracket_id,
            entry,
            entry_fill: None,
            initial_stop,
            resting_stop: initial_stop,
            resting_stop_qty: Quantity::ZERO,
            targets,
            remaining_targets,
            open_qty: Quantity::ZERO,
            trail,
            closed: false,
            events: Vec::new(),
        })
    }

    /// Register the entry fill and open the position.
    ///
    /// Sets `entry_fill`, `open_qty = fill.quantity` and
    /// `resting_stop_qty = open_qty`, then journals [`BracketEvent::EntryFilled`].
    ///
    /// # Errors
    /// [`OrderError::InvalidBracket`] when the bracket is closed, already
    /// filled, or the fill quantity does not match the entry order quantity;
    /// [`OrderError::InvalidParameters`] for a non-positive fill.
    pub fn on_entry_fill(
        &mut self,
        fill: &FillReport,
        now: DateTime<Utc>,
    ) -> Result<(), OrderError> {
        if self.closed {
            return Err(OrderError::InvalidBracket("bracket is closed".to_string()));
        }
        if self.entry_fill.is_some() {
            return Err(OrderError::InvalidBracket(
                "entry already filled".to_string(),
            ));
        }
        if !fill.quantity.is_positive() {
            return Err(OrderError::InvalidParameters);
        }
        if fill.quantity != self.entry.quantity {
            return Err(OrderError::InvalidBracket(format!(
                "entry fill quantity {fill_qty} must match the entry order quantity {entry_qty}",
                fill_qty = fill.quantity,
                entry_qty = self.entry.quantity
            )));
        }

        self.entry_fill = Some(fill.clone());
        self.open_qty = fill.quantity;
        self.resting_stop_qty = fill.quantity;
        self.events.push(BracketEvent::EntryFilled {
            price: fill.price,
            qty: fill.quantity,
            ts: now,
        });
        Ok(())
    }

    /// Modify the resting stop price and/or quantity.
    ///
    /// The stop may only **tighten**, never widen (mirrors `tantra-risk` gap
    /// 0005): a long stop must move up or hold (`new >= resting`), a short stop
    /// must move down or hold (`new <= resting`). `new_qty` must be non-negative
    /// and never exceed the remaining `open_qty` (no short-flip guard). On
    /// success the resting stop price/qty are updated and
    /// [`BracketEvent::StopMoved`] is journaled.
    ///
    /// # Errors
    /// [`OrderError::InvalidBracket`] when the bracket is closed;
    /// [`OrderError::StopWidening`] when the update would widen the stop;
    /// [`OrderError::StopQtyExceedsOpen`] when `new_qty > open_qty` or
    /// `new_qty` is negative.
    pub fn apply_stop_update(
        &mut self,
        new_stop: Price,
        new_qty: Option<Quantity>,
        now: DateTime<Utc>,
    ) -> Result<(), OrderError> {
        if self.closed {
            return Err(OrderError::InvalidBracket("bracket is closed".to_string()));
        }
        let widens = match self.entry.side {
            OrderSide::Buy => new_stop.value() < self.resting_stop.value(),
            OrderSide::Sell => new_stop.value() > self.resting_stop.value(),
        };
        if widens {
            return Err(OrderError::StopWidening {
                current: self.resting_stop,
                new: new_stop,
            });
        }
        if let Some(qty) = new_qty {
            let qty_value = qty.value();
            if qty_value < Decimal::ZERO || qty_value > self.open_qty.value() {
                return Err(OrderError::StopQtyExceedsOpen);
            }
            self.resting_stop_qty = qty;
        }

        let old = self.resting_stop;
        self.resting_stop = new_stop;
        self.events.push(BracketEvent::StopMoved {
            old,
            new: new_stop,
            qty: self.resting_stop_qty,
            ts: now,
        });
        Ok(())
    }

    /// Apply a partial-exit target fill, shrinking the runner.
    ///
    /// Shrinks `open_qty` by `qty` (never below zero), removes the matched
    /// [`PlannedTarget`] from `remaining_targets` (FIFO by position — the first
    /// remaining leg whose quantity equals the exit quantity), and journals
    /// [`BracketEvent::PartialExit`]. The bracket stays open for the runner.
    ///
    /// `reason` documents the exit leg (e.g. `"target 1 at 2R"`) for callers
    /// that drive the target matching themselves; the event journal itself does
    /// not carry it.
    ///
    /// # Errors
    /// [`OrderError::InvalidBracket`] when the bracket is closed;
    /// [`OrderError::InvalidParameters`] for a non-positive exit;
    /// [`OrderError::QuantityMismatch`] when the exit would push `open_qty`
    /// below zero (short flip).
    pub fn on_partial_exit(
        &mut self,
        price: Price,
        qty: Quantity,
        _reason: &str,
        now: DateTime<Utc>,
    ) -> Result<(), OrderError> {
        if self.closed {
            return Err(OrderError::InvalidBracket("bracket is closed".to_string()));
        }
        if !qty.is_positive() {
            return Err(OrderError::InvalidParameters);
        }
        let after = self.open_qty.value() - qty.value();
        if after < Decimal::ZERO {
            return Err(OrderError::QuantityMismatch);
        }
        if let Some(pos) = self.remaining_targets.iter().position(|t| t.qty == qty) {
            self.remaining_targets.remove(pos);
        }
        self.open_qty = Quantity::new(after);
        self.events.push(BracketEvent::PartialExit {
            price,
            qty,
            ts: now,
        });
        Ok(())
    }

    /// Trigger the resting stop and close the bracket.
    ///
    /// Closes the runner at the trigger `price`: `realized_pnl =
    /// (price − entry) × open_qty` for longs, `(entry − price) × open_qty` for
    /// shorts (exact `Decimal`), and journals [`BracketEvent::StopTriggered`]
    /// followed by [`BracketEvent::Closed`].
    ///
    /// # Errors
    /// [`OrderError::InvalidBracket`] when the bracket is closed or has no
    /// entry fill.
    pub fn on_stop_trigger(&mut self, price: Price, now: DateTime<Utc>) -> Result<(), OrderError> {
        if self.closed {
            return Err(OrderError::InvalidBracket("bracket is closed".to_string()));
        }
        let entry_price = self
            .entry_fill
            .as_ref()
            .ok_or_else(|| OrderError::InvalidBracket("bracket has no entry fill".to_string()))?
            .price
            .value();
        let qty = self.open_qty;
        let realized_pnl = match self.entry.side {
            OrderSide::Buy => (price.value() - entry_price) * qty.value(),
            OrderSide::Sell => (entry_price - price.value()) * qty.value(),
        };
        self.events.push(BracketEvent::StopTriggered {
            price,
            qty,
            ts: now,
        });
        self.mark_closed("stop_triggered".to_string(), realized_pnl, now);
        Ok(())
    }

    /// Dispatch an execution fill to the correct bracket leg.
    ///
    /// - A fill matching the entry (same client order id **and** the entry
    ///   quantity) routes to [`BracketOrder::on_entry_fill`].
    /// - A fill matching a remaining target leg (by quantity) routes to
    ///   [`BracketOrder::on_partial_exit`]; if that leaves `open_qty == 0` the
    ///   bracket is marked closed with the sum of all partial-exit PnL.
    /// - Any other fill is a no-op.
    ///
    /// # Errors
    /// Propagates the routed leg's errors. A closed bracket is an idempotent
    /// no-op (`Ok`).
    pub fn apply_fill_to_bracket(
        &mut self,
        fill: &FillReport,
        now: DateTime<Utc>,
    ) -> Result<(), OrderError> {
        if self.closed {
            return Ok(());
        }
        if fill.client_order_id == self.entry.client_order_id
            && fill.quantity == self.entry.quantity
        {
            return self.on_entry_fill(fill, now);
        }
        if self
            .remaining_targets
            .iter()
            .any(|t| t.qty == fill.quantity)
        {
            self.on_partial_exit(fill.price, fill.quantity, "target_leg", now)?;
            if self.open_qty.value() == Decimal::ZERO {
                let realized_pnl = self.realized_pnl_from_events();
                self.mark_closed("all_targets_exited".to_string(), realized_pnl, now);
            }
        }
        Ok(())
    }

    /// True once the bracket reached its terminal closed state.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Remaining position (the runner) after partial exits.
    #[must_use]
    pub const fn open_qty(&self) -> Quantity {
        self.open_qty
    }

    /// Current resting stop price.
    #[must_use]
    pub const fn resting_stop(&self) -> Price {
        self.resting_stop
    }

    /// Current resting stop quantity.
    #[must_use]
    pub const fn resting_stop_qty(&self) -> Quantity {
        self.resting_stop_qty
    }

    /// Append-only event journal.
    #[must_use]
    pub fn events(&self) -> &[BracketEvent] {
        &self.events
    }

    /// Quantity still open for the runner (alias of [`BracketOrder::open_qty`]).
    #[must_use]
    pub const fn remaining_qty(&self) -> Quantity {
        self.open_qty
    }

    /// Read-only projection for the [`crate::OmsSnapshot`] surface.
    #[must_use]
    pub fn view(&self) -> BracketView {
        BracketView {
            bracket_id: self.bracket_id.clone(),
            entry_price: self.entry_fill.as_ref().map(|f| f.price),
            resting_stop: self.resting_stop,
            resting_stop_qty: self.resting_stop_qty,
            open_qty: self.open_qty,
            target_count: self.remaining_targets.len(),
            tsl_active: self.trail.is_some(),
        }
    }

    /// Terminal close: mark closed, zero the position and stop, journal
    /// [`BracketEvent::Closed`].
    fn mark_closed(&mut self, reason: String, realized_pnl: Decimal, now: DateTime<Utc>) {
        self.closed = true;
        self.open_qty = Quantity::ZERO;
        self.resting_stop_qty = Quantity::ZERO;
        self.events.push(BracketEvent::Closed {
            reason,
            realized_pnl,
            ts: now,
        });
    }

    /// Sum the realized PnL of every exit event (`PartialExit` + `StopTriggered`).
    fn realized_pnl_from_events(&self) -> Decimal {
        let entry_price = match &self.entry_fill {
            Some(fill) => fill.price.value(),
            None => return Decimal::ZERO,
        };
        let per_unit = |price: Decimal| match self.entry.side {
            OrderSide::Buy => price - entry_price,
            OrderSide::Sell => entry_price - price,
        };
        self.events
            .iter()
            .fold(Decimal::ZERO, |acc, event| match event {
                BracketEvent::PartialExit { price, qty, .. }
                | BracketEvent::StopTriggered { price, qty, .. } => {
                    acc + per_unit(price.value()) * qty.value()
                }
                _ => acc,
            })
    }
}

// ———————————————————————————————————————————————————————————————————
// Snapshot projection
// ———————————————————————————————————————————————————————————————————

/// Read-only projection of an open bracket for the [`crate::OmsSnapshot`]
/// surface (UI and chat tool surface, gap 0012 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BracketView {
    /// The bracket aggregate identifier.
    pub bracket_id: BracketId,
    /// The entry fill price, once the position is open.
    pub entry_price: Option<Price>,
    /// Current resting stop price.
    pub resting_stop: Price,
    /// Current resting stop quantity.
    pub resting_stop_qty: Quantity,
    /// Remaining runner quantity.
    pub open_qty: Quantity,
    /// Number of target legs still pending exit.
    pub target_count: usize,
    /// True when the bracket carries a trailing-stop rule.
    pub tsl_active: bool,
}
