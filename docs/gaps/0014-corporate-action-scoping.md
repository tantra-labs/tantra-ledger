# 0014 — Corporate actions are not scoped to an instrument and are applied non-idempotently

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: critical
**Plan**: [0006](../plans/0006-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-ledger-portfolio/src/lib.rs:558-590` — `CorporateAction::Split`/`Bonus`/`Dividend` carry no `instrument` field (only `Demerger.new_instrument`, the *new* scrip). Orchestrator-verified: variants carry no instrument.
- `crates/tantra-ledger-portfolio/src/lib.rs:960-990` — `apply_corporate_actions_to_lots`: `for lot in &mut self.lots { adjust_lot_for_split(...) }` with no instrument filter; Dividend sums `amount_per_share * lot.remaining_quantity` over all lots.
- `crates/tantra-ledger-portfolio/src/lib.rs:947-958` — `apply_corporate_actions_to_lots` only skips actions with `ex_date > as_of` (`continue`), i.e. it applies every action with `ex_date <= as_of`: no instrument filter, no `lot.entry_date < ex_date` check (lots opened after the ex-date are scaled too) and no applied-action ledger.
- `crates/tantra-ledger-portfolio/src/lib.rs:839` — `apply_corporate_actions_to_position` re-applies every action with `ex_date <= as_of` on each call replay, restart or two `as_of` ticks apply a split twice.
- `crates/tantra-ledger-portfolio/src/lib.rs:651-655` / `:1000` — `amount_per_share` is "paise/minor units" (`i64`) but is multiplied into `Money::inr(...)` (rupees): 100x unit ambiguity.
- `crates/tantra-ledger-portfolio/src/lib.rs:765` — demerger lot id `DEMERGER_{lot_id}` collides on a second demerger.

## Why

A 1:5 split on RELIANCE silently divides the entry price of every TCS/INFY lot by 5 and multiplies its quantity. P&L, tax lots and cost basis in a live account become wrong with no error, and re-running the action feed after a restart doubles the effect. Introduced by closed gap 0008 (per-lot corporate actions). Breaks the DDD aggregate-invariant rule (`.agents/rules/coding-and-ddd.md` §1.2).

## How to close it

1. Add `instrument: InstrumentId` to every `CorporateAction` variant (required, no serde default; bump the wire version).
2. Filter lots by instrument and by `lot.entry_date < ex_date`.
3. Track applied actions (`BTreeSet<ActionId>` on `AccountState`; `ActionId` = hash of instrument + kind + ex_date) so re-application is a no-op.
4. Make dividend `Decimal` rupees per share (`Money`) instead of `i64` paise.
5. Use unique demerger lot ids (`{lot_id}:DM:{action_id}`).

**Verification**: tests in `tests/`: two-instrument account where a split affects only the target (proptest); idempotent double-apply; post-ex-date lot untouched; golden dividend in rupees. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `make check`, targeted `cargo test -p tantra-ledger-portfolio --profile prod`, `make prod`.

## Related

- Closed gap 0008 (introduced this code); [0021](0021-corporate-action-rounding.md) (fractional handling in the same functions); [0019](0019-event-log-and-snapshots.md) (action ids belong in the event log).
- Root gap 0019 (calendar, instrument master & corporate-action single sources).
