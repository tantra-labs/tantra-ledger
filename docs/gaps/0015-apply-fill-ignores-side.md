# 0015 — `apply_fill` ignores the fill side; a same-direction fill is booked as a close

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: high
**Plan**: [0006](../plans/0006-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-ledger-portfolio/src/lib.rs:1225-1231` — `apply_fill(pos, _side, fill_qty: u64, ..)`: `_side` is never read behaviour branches only on `pos.side`. A Buy on a Long position with `fill_qty <= pos.quantity` reduces quantity and books `(fill - entry) * qty` as realized P&L.
- `crates/tantra-ledger-portfolio/src/lib.rs:1281-1305` — `apply_fill_to_account` forwards to it and never creates `TaxLot`s, so the position book (average cost) and the `match_lots` lot book (FIFO) diverge.
- `tantra-runtime/crates/tantra-runtime-backtest/src/engine.rs:105-140` — `apply_fill_with_add_support` is a copy-paste workaround ("apply_fill would otherwise misinterpret as a closing fill").
- `tantra-runtime/crates/tantra-runtime-actors/src/portfolio.rs:140-148` — the live `PortfolioActor` calls `apply_fill_to_account` directly with no workaround, passing `Buy->Long`, `Sell->Short`, and `fill.quantity.0.try_into().unwrap_or(0)` (fractional/oversize quantity becomes 0).

## Why

A second Buy of an existing long in the live actor reduces the position and reports phantom realized P&L; backtest and live therefore disagree (research-live parity broken). It is a correctness bug today, separate from the FIFO bracket-P&L remainder of the closed plan 0005.

## How to close it

1. Make the aggregate own the rule: `AccountState::apply_fill(&mut self, fill: &LotFill, product) -> Result<FillOutcome, PortfolioError>` handling open / add (weighted average) / reduce / reverse using `OrderSide` + `Quantity` (Decimal, no `u64`).
2. Route through the lot book so positions are always the projection (as `rebuild_positions_from_lots` already does).
3. Delete the backtest workaround (`tantra-runtime-backtest/src/engine.rs:105`) and the runtime direct call (consumer fix in `tantra-runtime`, `PortfolioActor`).

**Verification**: add/reduce/reverse matrix as proptest (position qty == sum of lot remaining); golden fill sequence shared with the backtest. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p tantra-ledger-portfolio --profile prod`, `make prod`.

## Related

- [0020](0020-tax-lot-classification-and-dates.md) (lot product/date model that the new `apply_fill` must carry); plan 0006 R2 (cross-repo position model sprawl; root gap 0014).
- Consumer fix: `tantra-runtime` `PortfolioActor` and the backtest copy.
- Extends the FIFO remainder noted in closed plan 0005 / closed gap 0012 follow-on.
