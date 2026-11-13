# Gap 0012 — No Bracket-Order (SL/TSL) OMS Lifecycle

**Status**: open — tracked by root master plan [`docs/plans/0005-elephant-edge-strategy-platform.md`](../../../docs/plans/0005-elephant-edge-strategy-platform.md)

## What's Missing

The OMS state machine handles `New → Submitted → PartiallyFilled → Filled /
Canceled / Rejected` and `validate_order_against_context` (plan 0005), but it
has **no bracket-order lifecycle**: a parent entry order with **child** stop
and target orders, a **trailing-stop modify** path, and stop-trigger → close
semantics. The Elephant Edge (EE+) pipeline emits entry + `StopProtection`
(initial SL + TSL) at the strategy layer (`tantra-strategies` plan 0001),
`tantra-exec` gap 0003 executes the bracket, and the OMS must:

1. **Model the bracket**: `BracketOrder` aggregate = entry order + SL child
   order + optional target child order, with a shared `BracketTicket`.
2. **Journal TSL modifies**: every `modify_stop` (`StopUpdate` from exec) is a
   first-class OMS event — the resting SL price changes while the bracket stays
   alive, without creating a second "order".
3. **Close on stop/target**: SL child fills → bracket closes (reducing side);
   target fills → bracket closes; both feed `FillReport`s into the FIFO
   portfolio + journal exactly once (no double-count).
4. **Context validation**: bracket orders pass `validate_order_against_context`
   (notional / qty / collar / daily loss) *per bracket*, not per child — so the
   max-loss guard in `tantra-risk` gap 0005 is reflected in the OMS snapshot.

## Why

Without an OMS-level bracket aggregate, the platform would journal the SL child
as a separate trade, breaking FIFO tax-lot accounting (a stop exit is part of
the same position's lifecycle) and producing misleading `OmsSnapshot`s for
`tantra-ui` plan 0006 and the trade-performance journal. The bracket must be a
single domain aggregate so research-live parity and ledger integrity hold to
the paisa.

## How to Close It

1. `BracketOrder` aggregate + `BracketTicket` in `tantra-ledger-oms`,
   extending the existing `OrderStateMachine` (pure, zero-I/O, explicit
   `DateTime<Utc>`).
2. `apply_stop_update(bracket, StopUpdate)` — validates stop never widens
   (mirrors `tantra-risk` gap 0005), updates the resting SL, journals the event.
3. `on_fill(bracket, FillReport)` — routes the fill to the correct leg; a
   child fill closes the bracket and realizes P&L via the existing FIFO
   `portfolio.apply_fill`.
4. `OmsSnapshot` gains `open_brackets: Vec<BracketView { entry, sl, target,
   tsl }>` for the UI and chat tool surface.
5. Tests in `tests/` (TDD): full bracket lifecycle (entry → trail → stop
   trigger → close) with golden accounting vectors; no double-count assertion;
   stop-widening refusal.

## Verification

`cargo test --workspace --profile prod` green in `tantra-ledger`; a bracket
round-trip produces exactly one realized trade with the correct FIFO basis;
`make prod` green end-of-turn (build-discipline rule).
