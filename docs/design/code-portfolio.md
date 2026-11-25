# `tantra-ledger-portfolio` — FIFO Tax-Lot Portfolio Reference

Documents `crates/tantra-ledger-portfolio/src/lib.rs`: the FIFO tax-lot engine,
Indian T+1 Demat settlement, corporate actions, and portfolio valuation.

## `AccountState`

The portfolio aggregate root per trading account. Owns the canonical FIFO
tax-lot matching logic (`match_lots`), the lot book (`record_lot`), and
idempotent corporate-action tracking (`applied_corporate_actions: BTreeSet<ActionId>`) —
the repository layer only adapts canonical `FillReport`s into the matching input.

### `AccountState::match_lots` (FIFO matching)

When a closing fill arrives, `AccountState::match_lots(&LotFill)`:

1. Iterates open (non-exhausted) lots in FIFO order (oldest first).
2. Matches only *opposing* sides — a `Sell` fill closes `Long` lots, a `Buy`
   fill closes `Short` lots (Indian equity delivery semantics).
3. Returns `MatchedLot` entries with per-lot realized PnL, holding period, and
   long-term flag (`holding_period_days > 365`).
4. Rolls the aggregate's realized PnL and quantity onto the matching
   `HeldPosition`; matched quantity never exceeds the fill.

### Cost basis & PnL

- **Realized PnL**: computed per matched lot — `(exit − entry) × qty` for
  `Long`, `(entry − exit) × qty` for `Short` (all `rust_decimal::Decimal`).
- **Unrealized PnL**: computed against current marks for open positions.
- **Average entry price**: carried per `HeldPosition.entry_price`.

## Settlement & corporate actions

| Type / Function | Purpose |
|-----------------|---------|
| `ClearingCalendar` | NSE/BSE trading and settlement holidays; `is_trading_day`, `is_settlement_day`, `settlement_date` (T+1 business day), `next/prev_trading_day`. `new()` builds an empty weekend-only calendar (plan 0005, gap 0007 — the domain crate owns no holiday table); `with_holidays` injects the external holiday list that `tantra-runtime-backtest::NseTradingCalendar` provides from the `tantra-data` holiday data. |
| `Product` | Execution product taxonomy: `Delivery`, `Intraday`, `CoverOrder`, `BracketOrder`; `requires_settlement()` is true only for `Delivery`. |
| `DematHoldings` | Free vs blocked holdings, unsettled receivables/payables, `block_for_settlement`, `settle_receivable`. |
| `SettlementEngine` | Builds `SettlementInstruction`s from fills (`create_instruction`) and processes due settlements (`process_settlements`). |
| `Fill` / `SettlementInstruction` | Fill event and settlement instruction records (broker/execution layer shapes). |
| `CorporateAction` / `ActionId` | `CORPORATE_ACTION_WIRE_VERSION = 2`. `Split`, `Bonus`, `Dividend` (rupee `Money` per share), and `Demerger` — every variant carries a required `instrument: InstrumentId` (no serde default) and deterministic SHA-256 `ActionId`. Aggregate helpers and `AccountState::apply_corporate_actions_to_lots` filter strictly by `instrument == *action.instrument()` and `entry_date < action.ex_date()`, deduplicate via `AccountState::applied_corporate_actions`, and allocate collision-free `{lot_id}:DM:{action_id}` demerger lot IDs (plan 0006, gap 0014). |
| `settle_trades` | Trade-based T+1 settlement across a `PortfolioState` (cash movements, position changes). |
| `compute_capital_gains` / `CapitalGainType` / `TaxSummary` | FIFO capital gains with holding-period awareness; `TaxSummary::from_fills` builds a period summary applying corporate actions before FIFO cost basis. |

## Valuation & position math

| Function | Purpose |
|----------|---------|
| `Marks` | Market marks (instrument → last price) injected by the caller. |
| `value(state, marks)` | Deterministic `PortfolioSnapshot`: totals, per-symbol breakdown, unrealized PnL (against marks), realized PnL (every held position). |
| `apply_fill` / `apply_fill_to_account` | Legacy position-level fill application, including full-close-and-reverse (`Long`→`Short` and vice versa). |
| `init_position` | `HeldPosition` factory from a fill event. |
| `HeldPosition` | Position aggregate carrying `entry_price`, `entry_lot_ids`, `entry_date`, `realized_pnl`, caller-injected `updated_at`. |

## Key types

| Type | Purpose |
|------|---------|
| `TaxLot` | Canonical cost-basis lot: `lot_id`, `account_id`, `instrument`, `side`, `entry_price`, `quantity`, `remaining_quantity`, `entry_date`, `created_at`; `is_exhausted()`. |
| `MatchedLot` | Result of matching a fill against one tax lot (matched qty, entry/exit prices, realized PnL, holding period, long-term flag). |
| `LotFill` | Adapted fill input for portfolio matching (instrument, side, price, qty, timestamp). |
| `PortfolioState` | Multi-account state: `accounts: BTreeMap<String, AccountState>`, `base_currency`. |

## Zero-floats & determinism

All prices, quantities, and PnL values use `rust_decimal::Decimal` — no
`f32`/`f64` intermediates; settlement is computed down to the single paisa.
No clock and no I/O — the caller stamps `as_of`/`now`/`entry_date` so the same
positions and marks always give a byte-identical snapshot.

## Related

- `tantra-ledger/src/repository.rs` — `InMemoryPositionRepository` delegates
  `match_lots` / `record_lot` to the `AccountState` aggregate.
- `tantra-core/src/order.rs` — `FillReport` consumed as matching input.
- `tantra-core/src/types.rs` — `Price`, `Quantity`, `Money`, `Currency`.
- Tests: `apply_fill_tests.rs` (7), `bracket_fifo_tests.rs` (5),
  `clearing_calendar_injection_tests.rs` (7), `corporate_action_scoping_tests.rs` (7),
  `per_lot_corporate_action_tests.rs` (10), `unit_tests.rs` (23), plus
  `benches/portfolio_benchmarks.rs`.
