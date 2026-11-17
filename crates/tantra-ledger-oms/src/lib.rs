//! # व्यापार・閻 tantra-ledger-oms
//!
//! Synchronous, deterministic order-management state machine for Indian
//! equities. The engine never places an order directly: it validates, prices,
//! and forwards order requests to brokers (ADR 0032 §7). Here the OMS owns the
//! SEBI order lifecycle: idempotency by client order ID, strict legal state
//! transitions, and fill reconciliation against the confirmed target quantity.
//!
//! # Migration notes (Tier 1B, `honba-kokoro-oms` → `tantra-ledger-oms`)
//!
//! - Every duplicated type (`Exchange`, `InstrumentId`, `OrderSide::{Long,Short}`,
//!   `OrderType` flags, `TimeInForce::{GTC,IOC,FOK,GTT,GFD}`, `Currency`, `Money`,
//!   `Quantity(u64)`, `Price`, `OrderStatus::{Pending,Received,Partial}`) was
//!   deleted; the canonical [`tantra_core`] vocabulary is used exclusively.
//! - Submission and cancellation are pure synchronous state transitions, not
//!   `async` functions. Time is injected: every entry point takes an explicit
//!   `now: DateTime<Utc>`, and IDs are never minted with `Uuid::new_v4()`
//!   inside the domain (the caller-supplied request carries the correlation ID).
//! - `OrderState::PendingCancel` was dropped: the canonical
//!   [`OrderStatus`] vocabulary has no pre-cancellation state; cancellation
//!   transitions directly to `Cancelled` (SEBI audit streams carry cancellation
//!   *requests* as events, not order states).
//! - The obsolete OMS-local `CostBasisLot` was deleted; tax-lot accounting is
//!   owned by `tantra-ledger-portfolio` (`TaxLot` / `AccountState::match_lots`).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tantra_core::{Money, OrderAck, OrderRequest, Quantity};
use thiserror::Error;
use uuid::Uuid;

mod bracket;

pub use bracket::{BracketEvent, BracketId, BracketOrder, BracketView};

// ——————————————————————————————————————————————————————————————————————————
// Canonical vocabulary (re-exported so downstream gates import only this crate)
// ——————————————————————————————————————————————————————————————————————————

pub use tantra_core::{
    ClientOrderId, Currency, Exchange, FillReport, InstrumentId, OrderSide, OrderStatus, OrderType,
    Price, TimeInForce,
};

/// Hard per-order quantity ceiling (pre-trade sanity limit, portfolio-agnostic).
pub const MAX_QUANTITY_PER_ORDER: u64 = 1_000_000;

// ———————————————————————————————————————————————————————————————————
// Errors
// ———————————————————————————————————————————————————————————————————

/// Comprehensive order-layer error enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OrderError {
    /// Request quantity exceeds [`MAX_QUANTITY_PER_ORDER`].
    #[error("quantity {0} exceeds the per-order limit of {MAX_QUANTITY_PER_ORDER}")]
    QuantityExceedsLimit(Quantity),
    /// The daily realized loss breached the configured maximum.
    #[error("daily loss limit exceeded: today_pnl {today_pnl} < -{max_loss}")]
    DailyLossLimitExceeded {
        /// Realized PnL accumulated today.
        today_pnl: Decimal,
        /// Configured absolute daily loss ceiling.
        max_loss: Decimal,
    },
    /// Validation rejected the request (zero quantity, negative commission, ...).
    #[error("invalid parameters")]
    InvalidParameters,
    /// The referenced client order ID is unknown to the state machine.
    #[error("order not found: {0}")]
    OrderNotFound(ClientOrderId),
    /// The referenced order is in a terminal state and cannot receive fills or cancellations.
    #[error("order {order_id} is not active (status {status:?})")]
    OrderNotActive {
        /// The client order ID that was addressed.
        order_id: ClientOrderId,
        /// The terminal state it reached.
        status: OrderStatus,
    },
    /// Request notional (reference price × quantity) exceeds the configured
    /// per-order notional ceiling (`OmsConfig::max_notional_per_order`).
    #[error("notional {notional} exceeds the configured per-order limit of {max}")]
    NotionalExceedsLimit {
        /// The notional computed from the market reference price.
        notional: Decimal,
        /// The configured ceiling.
        max: Decimal,
    },
    /// The worst-case band-edge fill price deviates from the market reference
    /// beyond the configured price collar (`OmsConfig::price_collar_bps`).
    #[error(
        "price {requested} deviates from reference {reference} beyond the {collar_bps} bps collar"
    )]
    PriceCollarViolation {
        /// The band-edge price (ask for a buy, bid for a sell).
        requested: Decimal,
        /// The market reference price (last trade).
        reference: Decimal,
        /// The configured collar half-width in basis points.
        collar_bps: u32,
    },
    /// A fill would push cumulative filled quantity beyond the confirmed target.
    #[error("fill for {order_id} would exceed target {target} (projected {filled})")]
    FillExceedsTarget {
        /// The client order ID the fill belongs to.
        order_id: ClientOrderId,
        /// Confirmed target quantity.
        target: Quantity,
        /// The quantity that would have been cumulative after this fill.
        filled: Quantity,
    },
    /// The client order ID is already registered (idempotency violation).
    #[error("duplicate client order ID: {0}")]
    DuplicateClientOrderId(String),
    /// Broker / venue plumbing failure surfaced through the OMS.
    #[error("broker error: {0}")]
    BrokerError(String),
    /// A bracket-order aggregate failed construction or a lifecycle invariant
    /// (gap 0012): stop on the wrong side of the entry, non-positive target
    /// quantity, duplicate/re-mismatched entry fill, or an operation on a
    /// closed bracket.
    #[error("invalid bracket: {0}")]
    InvalidBracket(String),
    /// A trailing-stop update would widen the protective stop (mirrors
    /// `tantra-risk` gap 0005): a long stop may only tighten upward
    /// (`new >= current`), a short stop may only tighten downward
    /// (`new <= current`).
    #[error("stop widening refused: {new} would widen the current stop {current}")]
    StopWidening {
        /// The resting stop before the update.
        current: Price,
        /// The requested stop that would widen the protective level.
        new: Price,
    },
    /// A stop-quantity update exceeds the remaining open quantity (or is
    /// negative) — a resting stop over-selling the closed partial quantity
    /// would flip the net position short.
    #[error("stop quantity must be non-negative and at most the open quantity")]
    StopQtyExceedsOpen,
    /// A partial exit would push the remaining open quantity below zero
    /// (short-flip guard).
    #[error("quantity mismatch: partial exit exceeds the open quantity")]
    QuantityMismatch,
}

/// Attempted an illegal order-state transition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransitionError {
    /// The transition `(from -> to)` is not part of the legal SEBI lifecycle graph.
    #[error("illegal transition {from:?} -> {to:?}")]
    Illegal {
        /// Source state.
        from: OrderStatus,
        /// Target state.
        to: OrderStatus,
    },
    /// The client order ID is already registered.
    #[error("duplicate client order ID: {client_order_id}")]
    DuplicateClientOrderId {
        /// The offending client order ID.
        client_order_id: String,
    },
}

// ———————————————————————————————————————————————————————————————————
// OMS context types (canvas the risk gate and broker adapters fill in)
// ———————————————————————————————————————————————————————————————————

/// Market data snapshot used for price-collar validation around submission.
#[derive(Debug, Clone)]
pub struct MarketData {
    /// Instrument the quotes belong to.
    pub instrument: InstrumentId,
    /// Best bid.
    pub bid: Price,
    /// Best ask.
    pub ask: Price,
    /// Last traded price.
    pub last: Price,
    /// Volume accumulated for the day so far.
    pub volume: Quantity,
    /// Quote timestamp, supplied by the caller (never minted here).
    pub timestamp: DateTime<Utc>,
}

/// OMS configuration knobs.
#[derive(Debug, Clone)]
pub struct OmsConfig {
    /// Maximum notional value per single order, if configured.
    pub max_notional_per_order: Option<Decimal>,
    /// Maximum quantity per single order, if configured (falls back to
    /// [`MAX_QUANTITY_PER_ORDER`]).
    pub max_quantity_per_order: Option<Quantity>,
    /// Maximum absolute daily realized loss, if configured.
    pub max_daily_loss: Option<Money>,
    /// Price-collar half-width in basis points, if configured.
    pub price_collar_bps: Option<u32>,
    /// Time in force applied when a request omits one.
    pub default_time_in_force: TimeInForce,
}

/// Point-in-time snapshot of OMS-wide counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OmsSnapshot {
    /// Orders registered so far.
    pub total_orders: u64,
    /// Orders that reached `Filled`.
    pub total_filled: u64,
    /// Orders that reached `Cancelled`.
    pub total_cancelled: u64,
    /// Commission charged so far.
    pub total_commission: Money,
    /// Statutory/other fees charged so far.
    pub total_fees: Money,
    /// Realized PnL today.
    pub today_pnl: Money,
    /// Open bracket-order aggregates (gap 0012), for the UI and chat tool
    /// surface. Empty when the OMS tracks no open brackets.
    #[serde(default)]
    pub open_brackets: Vec<BracketView>,
}

impl Default for OmsSnapshot {
    fn default() -> Self {
        Self {
            total_orders: 0,
            total_filled: 0,
            total_cancelled: 0,
            total_commission: Money::inr(Decimal::ZERO),
            total_fees: Money::inr(Decimal::ZERO),
            today_pnl: Money::inr(Decimal::ZERO),
            open_brackets: Vec::new(),
        }
    }
}

/// Portfolio state consumed by pre-submission risk checks.
#[derive(Debug, Clone)]
pub struct PortfolioRiskState {
    /// Free cash available for new orders.
    pub cash: Money,
    /// Net quantity held per instrument.
    pub positions: BTreeMap<InstrumentId, Quantity>,
    /// Realized PnL accumulated today.
    pub today_pnl: Money,
    /// Absolute daily loss limit.
    pub max_daily_loss: Money,
    /// Price-collar half-width in basis points, if configured.
    pub price_collar_bps: Option<u32>,
}

// ———————————————————————————————————————————————————————————————————
// Order lifecycle aggregate
// ———————————————————————————————————————————————————————————————————

/// One tracked order inside [`OrderStateMachine`].
///
/// The entry carries *both* identifier spellings of the same order: the
/// canonical string key ([`ClientOrderId`]) and the correlation UUID embedded
/// in the wire request, so acknowledgements can be built without re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderEntry {
    /// Canonical client order identifier (state-machine key).
    pub client_order_id: ClientOrderId,
    /// Correlation UUID carried by the originating [`OrderRequest`].
    pub client_order_uuid: Uuid,
    /// Platform-assigned order identifier (deterministic, derived from the request).
    pub order_id: String,
    /// Current lifecycle state.
    pub state: OrderStatus,
    /// Confirmed target quantity from the submitted request.
    pub target_quantity: Quantity,
    /// Cumulative filled quantity (never exceeds [`OrderEntry::target_quantity`]).
    pub filled_quantity: Quantity,
    /// Creation timestamp injected by the caller.
    pub created_at: DateTime<Utc>,
    /// Last transition timestamp injected by the caller.
    pub updated_at: DateTime<Utc>,
}

impl OrderEntry {
    /// Convenience for constructing a fresh (unfilled) entry.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client_order_id: ClientOrderId,
        client_order_uuid: Uuid,
        order_id: impl Into<String>,
        initial_state: OrderStatus,
        target_quantity: Quantity,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            client_order_id,
            client_order_uuid,
            order_id: order_id.into(),
            state: initial_state,
            target_quantity,
            filled_quantity: Quantity::ZERO,
            created_at,
            updated_at: created_at,
        }
    }

    /// Quantity still pending execution.
    #[must_use]
    pub fn remaining_quantity(&self) -> Quantity {
        Quantity::new(
            (self.target_quantity.value() - self.filled_quantity.value()).max(Decimal::ZERO),
        )
    }

    /// True once the cumulative fill reaches the target quantity.
    #[must_use]
    pub fn is_fully_filled(&self) -> bool {
        self.state == OrderStatus::Filled
    }

    /// True if no further fills or cancellations are applicable.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }
}

/// Order state machine managing the SEBI order lifecycle with idempotency and
/// fill reconciliation.
///
/// - Keys are canonical [`ClientOrderId`]s (idempotency guaranteed per order).
/// - [`OrderStateMachine::process_fill`] transitions to `PartiallyFilled` while
///   cumulative fills remain below the target and to `Filled` once the target
///   is reached; fills beyond the target are strictly rejected so the
///   cumulative counter can never be double-counted.
/// - Terminal states are absorbing: no transition out is legal.
#[derive(Debug, Clone)]
pub struct OrderStateMachine {
    orders: BTreeMap<ClientOrderId, OrderEntry>,
}

impl Default for OrderStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderStateMachine {
    /// Create a new order state machine.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            orders: BTreeMap::new(),
        }
    }

    /// Validate the transition `from -> to` against the legal lifecycle graph.
    ///
    /// Returns `Ok(to)` if legal, `Err(TransitionError::Illegal { from, to })`
    /// otherwise. Terminal states are absorbing (`Filled|Cancelled|Rejected|Expired
    /// -> itself` is legal and idempotent; anything else out of them is illegal).
    ///
    /// The `Result` must be handled: ignoring a rejection would silently
    /// corrupt the lifecycle.
    pub fn transition(from: OrderStatus, to: OrderStatus) -> Result<OrderStatus, TransitionError> {
        use OrderStatus::*;
        let ok = matches!(
            (from, to),
            (PendingNew, New)
                | (PendingNew, Rejected)
                | (PendingNew, Cancelled)
                // A first fill may land before the acknowledgement is processed.
                | (PendingNew, PartiallyFilled)
                | (New, PartiallyFilled)
                | (New, Filled)
                | (New, Cancelled)
                | (New, Rejected)
                | (PartiallyFilled, Filled)
                | (PartiallyFilled, Cancelled)
                // Terminal states are absorbing.
                | (Filled, Filled)
                | (Cancelled, Cancelled)
                | (Rejected, Rejected)
                | (Expired, Expired)
        );
        if ok {
            Ok(to)
        } else {
            Err(TransitionError::Illegal { from, to })
        }
    }

    /// Check idempotency: if the client order ID already exists, return error.
    /// If new, return `Ok`.
    ///
    /// # Errors
    /// [`TransitionError::DuplicateClientOrderId`] when the ID is registered.
    pub fn check_idempotency(
        &self,
        client_order_id: &ClientOrderId,
    ) -> Result<(), TransitionError> {
        if self.orders.contains_key(client_order_id) {
            Err(TransitionError::DuplicateClientOrderId {
                client_order_id: client_order_id.0.clone(),
            })
        } else {
            Ok(())
        }
    }

    /// Register a fully-formed order entry.
    ///
    /// # Errors
    /// [`TransitionError::DuplicateClientOrderId`] when the key already exists.
    pub fn register_order(&mut self, entry: OrderEntry) -> Result<(), TransitionError> {
        self.check_idempotency(&entry.client_order_id)?;
        self.orders.insert(entry.client_order_id.clone(), entry);
        Ok(())
    }

    /// Register a new order with a validated state, target quantity, and
    /// correlation identifiers.
    ///
    /// Thin convenience over [`OrderStateMachine::register_order`] for call
    /// sites that hold the request identifiers directly.
    ///
    /// # Errors
    /// [`TransitionError::DuplicateClientOrderId`] when the key already exists.
    #[allow(clippy::too_many_arguments)]
    pub fn register_request_ids(
        &mut self,
        client_order_id: ClientOrderId,
        client_order_uuid: Uuid,
        order_id: impl Into<String>,
        initial_state: OrderStatus,
        target_quantity: Quantity,
        created_at: DateTime<Utc>,
    ) -> Result<(), TransitionError> {
        let entry = OrderEntry::new(
            client_order_id,
            client_order_uuid,
            order_id,
            initial_state,
            target_quantity,
            created_at,
        );
        self.register_order(entry)
    }

    /// Validate and register an order request, returning its acknowledgement.
    ///
    /// Pure synchronous state transition: no I/O, no clock, no UUID minting.
    /// The order ID is derived deterministically from the request correlation
    /// UUID; time comes from the `now` parameter.
    ///
    /// # Errors
    /// [`OrderError`]: quantity/validation rejection or duplicate registration.
    pub fn submit_order(
        &mut self,
        request: &OrderRequest,
        now: DateTime<Utc>,
    ) -> Result<OrderAck, OrderError> {
        validate_order_request(request)?;
        let client_order_uuid = request.client_order_id;
        let client_order_id = ClientOrderId::from_uuid(client_order_uuid);
        let order_id = platform_order_id(client_order_uuid);
        self.register_request_ids(
            client_order_id,
            client_order_uuid,
            order_id.clone(),
            OrderStatus::PendingNew,
            request.quantity,
            now,
        )
        .map_err(|e| match e {
            TransitionError::DuplicateClientOrderId { client_order_id } => {
                OrderError::DuplicateClientOrderId(client_order_id)
            }
            other => OrderError::BrokerError(other.to_string()),
        })?;
        Ok(build_order_ack(request, now))
    }

    /// Submit an order request validated against the OMS context canvas
    /// (gap 0011): `OmsConfig`-driven `max_notional_per_order` /
    /// `max_quantity_per_order` / `price_collar_bps` checks against
    /// `MarketData`, and the configured daily-loss ceiling against
    /// `PortfolioRiskState`, before the normal `submit_order` registration.
    ///
    /// # Errors
    /// [`OrderError::NotionalExceedsLimit`],
    /// [`OrderError::PriceCollarViolation`],
    /// [`OrderError::QuantityExceedsLimit`],
    /// [`OrderError::DailyLossLimitExceeded`], plus every `submit_order` error.
    pub fn submit_order_with_context(
        &mut self,
        request: &OrderRequest,
        now: DateTime<Utc>,
        config: &OmsConfig,
        market: Option<&MarketData>,
        risk: Option<&PortfolioRiskState>,
    ) -> Result<OrderAck, OrderError> {
        validate_order_against_context(request, config, market, risk)?;
        self.submit_order(request, now)
    }

    /// Point-in-time snapshot of the OMS counters, maintained from the live
    /// order book (gap 0011): `total_orders` / `total_filled` /
    /// `total_cancelled` are derived from the tracked entries. Commission,
    /// fees and today's PnL are zero until a real charge/PnL feed exists — the
    /// type is now consumed rather than dead code.
    #[must_use]
    pub fn snapshot(&self) -> OmsSnapshot {
        OmsSnapshot {
            total_orders: self.orders.len() as u64,
            total_filled: self
                .orders
                .values()
                .filter(|o| o.state == OrderStatus::Filled)
                .count() as u64,
            total_cancelled: self
                .orders
                .values()
                .filter(|o| o.state == OrderStatus::Cancelled)
                .count() as u64,
            total_commission: Money::inr(Decimal::ZERO),
            total_fees: Money::inr(Decimal::ZERO),
            today_pnl: Money::inr(Decimal::ZERO),
            open_brackets: Vec::new(),
        }
    }

    /// Cancel an active order. Terminal states are absorbing, so cancelling a
    /// `Filled` / `Rejected` / `Expired` order is invalid.
    ///
    /// # Errors
    /// [`OrderError::OrderNotFound`] when unknown,
    /// [`OrderError::OrderNotActive`] when already terminal.
    pub fn cancel_order(
        &mut self,
        client_order_id: &ClientOrderId,
        now: DateTime<Utc>,
    ) -> Result<OrderAck, OrderError> {
        let entry = self
            .orders
            .get_mut(client_order_id)
            .ok_or_else(|| OrderError::OrderNotFound(client_order_id.clone()))?;
        if entry.is_terminal() {
            return Err(OrderError::OrderNotActive {
                order_id: client_order_id.clone(),
                status: entry.state,
            });
        }
        let from = entry.state;
        OrderStateMachine::transition(from, OrderStatus::Cancelled).map_err(|_| {
            OrderError::BrokerError(format!("illegal transition {from:?} -> Cancelled"))
        })?;
        entry.state = OrderStatus::Cancelled;
        entry.updated_at = now;
        Ok(OrderAck::new(
            entry.order_id.clone(),
            entry.client_order_uuid,
            OrderStatus::Cancelled,
            now,
        ))
    }

    /// Process a fill event: reconcile against the target quantity, transition
    /// the state, and return `(state, fully_filled)`.
    ///
    /// `fully_filled` is true exactly when the cumulative fill reaches the
    /// confirmed target. Fills that would overshoot the target are rejected
    /// (never silently saturated) so reconciliation stays auditable.
    ///
    /// # Errors
    /// [`OrderError::OrderNotFound`], [`OrderError::OrderNotActive`],
    /// [`OrderError::InvalidParameters`] (non-positive fill),
    /// [`OrderError::FillExceedsTarget`].
    pub fn process_fill(
        &mut self,
        client_order_id: &ClientOrderId,
        fill_quantity: Quantity,
        now: DateTime<Utc>,
    ) -> Result<(OrderStatus, bool), OrderError> {
        let entry = self
            .orders
            .get_mut(client_order_id)
            .ok_or_else(|| OrderError::OrderNotFound(client_order_id.clone()))?;
        if entry.is_terminal() {
            return Err(OrderError::OrderNotActive {
                order_id: client_order_id.clone(),
                status: entry.state,
            });
        }
        if fill_quantity.value() <= Decimal::ZERO {
            return Err(OrderError::InvalidParameters);
        }
        let projected = entry.filled_quantity.value() + fill_quantity.value();
        let target = entry.target_quantity.value();
        if projected > target {
            return Err(OrderError::FillExceedsTarget {
                order_id: client_order_id.clone(),
                target: entry.target_quantity,
                filled: Quantity::new(projected),
            });
        }

        entry.filled_quantity = Quantity::new(projected);
        entry.updated_at = now;
        let state = if projected >= target {
            OrderStatus::Filled
        } else {
            OrderStatus::PartiallyFilled
        };
        entry.state = state;
        Ok((state, state == OrderStatus::Filled))
    }

    /// Get the current state of an order.
    #[must_use]
    pub fn get_state(&self, client_order_id: &ClientOrderId) -> Option<OrderStatus> {
        self.orders.get(client_order_id).map(|e| e.state)
    }

    /// Get the cumulative filled quantity for an order.
    #[must_use]
    pub fn get_cumulative_fill(&self, client_order_id: &ClientOrderId) -> Option<Quantity> {
        self.orders.get(client_order_id).map(|e| e.filled_quantity)
    }

    /// Get the confirmed target quantity for an order.
    #[must_use]
    pub fn get_target_quantity(&self, client_order_id: &ClientOrderId) -> Option<Quantity> {
        self.orders.get(client_order_id).map(|e| e.target_quantity)
    }

    /// Check if an order is in a terminal state.
    #[must_use]
    pub fn is_terminal(&self, client_order_id: &ClientOrderId) -> bool {
        self.orders
            .get(client_order_id)
            .is_some_and(OrderEntry::is_terminal)
    }

    /// Get all client order IDs, ordered (BTreeMap iteration).
    #[must_use]
    pub fn all_order_ids(&self) -> Vec<ClientOrderId> {
        self.orders.keys().cloned().collect()
    }
}

// ———————————————————————————————————————————————————————————————————
// Pure validation and acknowledgement builders
// ———————————————————————————————————————————————————————————————————

/// Validate an order request against OMS sanity limits.
///
/// # Errors
/// - [`OrderError::InvalidParameters`] for a zero quantity.
/// - [`OrderError::QuantityExceedsLimit`] above [`MAX_QUANTITY_PER_ORDER`].
pub fn validate_order_request(request: &OrderRequest) -> Result<(), OrderError> {
    let qty = request.quantity.value();
    if qty <= Decimal::ZERO {
        return Err(OrderError::InvalidParameters);
    }
    let max = Decimal::from(MAX_QUANTITY_PER_ORDER);
    if qty > max {
        return Err(OrderError::QuantityExceedsLimit(request.quantity));
    }
    Ok(())
}

/// Validate order parameters against the per-order ceiling and the daily loss limit.
///
/// # Errors
/// - [`OrderError::QuantityExceedsLimit`] above [`MAX_QUANTITY_PER_ORDER`].
/// - [`OrderError::DailyLossLimitExceeded`] when `today_pnl < -max_loss`.
pub fn validate_order_params(
    qty: Quantity,
    today_pnl: Decimal,
    max_loss: Decimal,
) -> Result<(), OrderError> {
    let max = Decimal::from(MAX_QUANTITY_PER_ORDER);
    if qty.value() > max {
        return Err(OrderError::QuantityExceedsLimit(qty));
    }
    if today_pnl < -max_loss {
        return Err(OrderError::DailyLossLimitExceeded {
            today_pnl,
            max_loss,
        });
    }
    Ok(())
}

/// Validate an order request against the OMS context canvas (gap 0011):
/// the base [`validate_order_request`] checks plus the configurable
/// `OmsConfig` controls that the previous canvas types declared but never
/// enforced.
///
/// 1. **`max_quantity_per_order`** — the configured per-order ceiling
///    (stricter than [`MAX_QUANTITY_PER_ORDER`]).
/// 2. **`max_notional_per_order`** — reference price (ask for a buy, bid for
///    a sell, else last) × quantity, checked against the configured ceiling.
/// 3. **`price_collar_bps`** — the worst-case band-edge fill (ask for a buy,
///    bid for a sell) must stay within `reference ± collar` of the last
///    price, mirroring the `tantra-risk` price-collar contract
///    (`docs/design/0002-reduce-only-contract.md`).
/// 4. **`max_daily_loss`** — `PortfolioRiskState::today_pnl` below
///    `-max_daily_loss` refuses the order.
///
/// When `market` is supplied it must be for the request's instrument,
/// otherwise the request is rejected ([`OrderError::InvalidParameters`]).
///
/// # Errors
/// - [`OrderError::InvalidParameters`] for a zero quantity or a market-data
///   snapshot of a different instrument.
/// - [`OrderError::QuantityExceedsLimit`] above the configured ceiling.
/// - [`OrderError::NotionalExceedsLimit`] above the configured notional cap.
/// - [`OrderError::PriceCollarViolation`] outside the configured collar.
/// - [`OrderError::DailyLossLimitExceeded`] on the daily-loss breach.
pub fn validate_order_against_context(
    request: &OrderRequest,
    config: &OmsConfig,
    market: Option<&MarketData>,
    risk: Option<&PortfolioRiskState>,
) -> Result<(), OrderError> {
    validate_order_request(request)?;

    let qty = request.quantity.value();
    if let Some(max_qty) = config.max_quantity_per_order
        && qty > max_qty.value()
    {
        return Err(OrderError::QuantityExceedsLimit(request.quantity));
    }

    if let Some(md) = market {
        if md.instrument != request.instrument {
            return Err(OrderError::InvalidParameters);
        }
        if let Some(max_notional) = config.max_notional_per_order {
            let reference = match request.side {
                OrderSide::Buy => {
                    if md.ask.value() > Decimal::ZERO {
                        md.ask.value()
                    } else {
                        md.last.value()
                    }
                }
                OrderSide::Sell => {
                    if md.bid.value() > Decimal::ZERO {
                        md.bid.value()
                    } else {
                        md.last.value()
                    }
                }
            };
            let notional = reference * qty;
            if notional > max_notional {
                return Err(OrderError::NotionalExceedsLimit {
                    notional,
                    max: max_notional,
                });
            }
        }
        if let Some(collar_bps) = config.price_collar_bps
            && collar_bps > 0
            && md.last.value() > Decimal::ZERO
        {
            let reference = md.last.value();
            let band_edge = match request.side {
                OrderSide::Buy => {
                    if md.ask.value() > Decimal::ZERO {
                        md.ask.value()
                    } else {
                        reference
                    }
                }
                OrderSide::Sell => {
                    if md.bid.value() > Decimal::ZERO {
                        md.bid.value()
                    } else {
                        reference
                    }
                }
            };
            let max_move = reference * Decimal::from(collar_bps) / Decimal::from(10_000);
            if (band_edge - reference).abs() > max_move {
                return Err(OrderError::PriceCollarViolation {
                    requested: band_edge,
                    reference,
                    collar_bps,
                });
            }
        }
    }

    if let Some(r) = risk
        && let Some(max_loss) = config.max_daily_loss
        && r.today_pnl.amount < -max_loss.amount
    {
        return Err(OrderError::DailyLossLimitExceeded {
            today_pnl: r.today_pnl.amount,
            max_loss: max_loss.amount,
        });
    }

    Ok(())
}

/// Deterministic platform order ID: `OMS-<client correlation uuid>`.
#[must_use]
pub fn platform_order_id(client_order_uuid: Uuid) -> String {
    format!("OMS-{client_order_uuid}")
}

/// Core order-acknowledgement construction (formerly the private
/// `_build_order_ack_inner`).
///
/// The acknowledged status is always [`OrderStatus::PendingNew`]: submission
/// registers the order as locally pending; the broker acknowledgement is a
/// separate lifecycle event.
///
/// # Errors
/// Propagates [`validate_order_request`] rejections.
pub fn submit_order(request: &OrderRequest, now: DateTime<Utc>) -> Result<OrderAck, OrderError> {
    validate_order_request(request)?;
    Ok(build_order_ack(request, now))
}

/// Build the deterministic acknowledgement for a validated request.
///
/// No clock and no UUID minting happen here: the order ID derives from the
/// request's correlation UUID and `now` is the caller-supplied timestamp.
#[must_use]
pub fn build_order_ack(request: &OrderRequest, now: DateTime<Utc>) -> OrderAck {
    let client_order_uuid = request.client_order_id;
    OrderAck::new(
        platform_order_id(client_order_uuid),
        client_order_uuid,
        OrderStatus::PendingNew,
        now,
    )
}

/// Validate a fill report's sanity before reconciliation.
///
/// # Errors
/// - [`OrderError::InvalidParameters`] for a zero fill or negative commission.
pub fn validate_fill_report(
    fill_quantity: Quantity,
    commission: Money,
    _fees: Money,
) -> Result<(), OrderError> {
    if fill_quantity.value() <= Decimal::ZERO {
        return Err(OrderError::InvalidParameters);
    }
    if commission.amount < Decimal::ZERO {
        return Err(OrderError::InvalidParameters);
    }
    Ok(())
}
