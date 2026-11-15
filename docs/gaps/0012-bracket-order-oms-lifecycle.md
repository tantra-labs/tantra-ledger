# Gap 0012 — No Bracket-Order (SL / Multi-Leg Target / TSL) OMS Lifecycle

**Status**: open — tracked by root master plans [`docs/plans/0005-elephant-edge-strategy-platform.md`](../../../docs/plans/0005-elephant-edge-strategy-platform.md) and [`docs/plans/0008-trade-signal-generator.md`](../../../docs/plans/0008-trade-signal-generator.md)

## What's Missing

The OMS state machine handles `New → Submitted → PartiallyFilled → Filled /
Canceled / Rejected` and `validate_order_against_context` (plan 0005), but it
has **no bracket-order lifecycle**: a parent entry order with a **child stop**
order and **zero or more partial-exit target legs** (`TargetLadder`), a
**trailing-stop modify** path (price + remaining-qty shrink), and N-leg
scale-out → close semantics. `TradePlan` (core plan 0005) emits entry +
`initial_stop` + `targets: Vec<PlannedTarget>` + `trail`; `tantra-exec` gap
0003 (and `tantra-runtime` plan 0006 synthetic OCO) execute the legs, and the OMS must:

1. **Model the multi-leg bracket**: `BracketOrder` aggregate = entry order +
   resting SL child order + `Vec<PlannedTarget>` child/pending exit legs +
   `remaining_qty`, with a shared `BracketTicket`. (A 1-to-1 single-target model
   would prematurely close the bracket on the first `PartialExit`!)
2. **Journal TSL & qty modifies**: every `MoveStop` (`modify_stop`) or
   partial-exit SL quantity shrink is a first-class OMS event — updating the
   resting SL price/qty while the bracket stays open for the runner, without
   creating an orphaned second "order".
3. **Partial exits & close on remaining zero**: each `PartialExit` or partial
   target fill feeds its `FillReport` into FIFO `portfolio.apply_fill`, shrinks
   `remaining_qty` (and validates that the resting SL qty shrinks to match,
   preventing short-flips), and closes the bracket only when `remaining_qty == 0`
   (stop trigger, final target, or time-stop `ExitAll`).
4. **Context validation**: bracket orders pass `validate_order_against_context`
   (notional / qty / collar / weight / ADV / daily loss) *per bracket*, not per
   child — so the max-loss guard in `tantra-risk` gap 0005 is reflected in the OMS snapshot.

## Why

Without an N-leg OMS bracket aggregate, the platform would either reject
scale-outs, prematurely mark a trade closed on the first ⅓ partial exit, allow
the resting SL to over-sell the closed partial quantity (flipping net position
short), or break FIFO tax-lot accounting and R-multiple journaling. The bracket
must be a single domain aggregate so research-live parity and ledger integrity
hold to the paisa.

## How to Close It

1. `BracketOrder` aggregate + `BracketTicket` in `tantra-ledger-oms`,
   extending `OrderStateMachine` (pure, zero-I/O, explicit `DateTime<Utc>`),
   consuming `TradePlan` / `TradeAction` from `tantra-core` plan `0005`.
2. `apply_stop_update(bracket, new_stop, new_qty?)` — validates stop never widens
   (mirrors `tantra-risk` gap 0005) and never exceeds `remaining_qty`, updates
   the resting SL, and journals the event.
3. `on_fill(bracket, FillReport)` — routes fills to entry or exit legs; realizes
   partial and runner P&L via FIFO `portfolio.apply_fill`, cancels residual open
   child legs when `remaining_qty == 0`, and records per-leg R-multiples.
4. `OmsSnapshot` gains `open_brackets: Vec<BracketView { entry, sl, remaining_qty, targets, tsl }>`
   for the UI and chat tool surface.
5. Tests in `tests/` (TDD): full multi-leg lifecycle (entry → partial exit 1/3 +
   SL resize → trail → stop trigger on runner → close) with golden FIFO vectors;
   no double-count and no short-flip assertions; stop-widening refusal.

## Verification

`cargo test --workspace --profile prod` green in `tantra-ledger`; a multi-leg
bracket round-trip produces exact FIFO realized P&L across partials + runner;
`make prod` green end-of-turn (build-discipline rule).
