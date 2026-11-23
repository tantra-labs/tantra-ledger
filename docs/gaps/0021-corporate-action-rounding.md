# 0021 — Fractional-share handling at split / bonus / demerger is unspecified

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: medium
**Plan**: [0006](../plans/0006-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-ledger-portfolio/src/lib.rs:707-737` — `adjust_lot_for_split/bonus` scale quantity by `num/den` and divide entry price by the same `Decimal` ratio; a 1:3 bonus gives 4/3 quantity multipliers and 28-digit repeating prices; no `round_dp`, no whole-share floor.
- `crates/tantra-ledger-portfolio/src/lib.rs:742-775` — demerger quantity = `lot.quantity * alloc_ratio` with no fractional-entitlement cash-in-lieu handling (Indian practice: fractions are aggregated and sold; proceeds are a cash event).
- `match_lots` (`crates/tantra-ledger-portfolio/src/lib.rs:1071+`) does exact Decimal splitting at lot boundaries with no rounding of `entry_price * qty` per lot; per-lot costs can differ from the original basis by repeating-decimal dust, and lots carry `remaining_quantity` like `3.3333333333333333333333333333`.

## Why

Positions with non-integral quantities leak into `Quantity` consumers that expect whole shares (e.g. the runtime `u64` casts in [0015](0015-apply-fill-ignores-side.md)), and basis dust accumulates in the journal when realized P&L is posted. Confidence is medium: the exchange rounding rule per action type must be confirmed with the corporate-action provider.

## How to close it

1. Define a `CorporateActionRounding` policy: floor to whole shares; the fraction becomes a `CashInLieu` event with a price.
2. Store lot total cost basis (`Money`) rather than a derived per-unit price, so splits preserve basis exactly; round prices only at presentation.
3. Apply the policy in `adjust_lot_for_split/bonus/demerger`.

**Verification**: proptest "total basis invariant under split/bonus"; golden bonus 1:3 vector in `tests/`. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p tantra-ledger-portfolio --profile prod`, `make prod`.

## Related

- [0014](0014-corporate-action-scoping.md) (same functions; land first), [0015](0015-apply-fill-ignores-side.md); root gap 0019 (corporate-action single source).
