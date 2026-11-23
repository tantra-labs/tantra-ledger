# 0020 — Lot book lacks product classification; holding period hard-coded and UTC-dated

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: medium
**Plan**: [0006](../plans/0006-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-ledger-portfolio/src/lib.rs:476-486` — `TaxLot` has no `Product` / segment / speculative flag; `match_lots` (`:1071-1140`) matches any lot of the instrument regardless of product, so an intraday MIS sell closes an old delivery lot (wrong STT, wrong STCG vs speculative-business-income classification).
- `crates/tantra-ledger-portfolio/src/lib.rs:1120` — `is_long_term = holding_days > 365`, and the same constant at `:1550`, for every asset class (listed derivatives, debt MF and ETFs have other thresholds). Holding days use `fill.timestamp.date_naive()` (UTC) while `entry_date` is caller-supplied (IST): dates drift for late-evening IST fills.
- `crates/tantra-ledger-portfolio/src/lib.rs:1537-1560` `compute_capital_gains` and `TaxSummary::from_fills` (`:1586-1646`) use the position-level average `entry_price`/`entry_date`, ignore `_side` and do not consume lots, so FIFO P&L (`match_lots`) and tax P&L differ for multi-lot positions.
- `crates/tantra-ledger-portfolio/src/lib.rs:527-537` — `HeldPosition::new` stamps a sentinel `entry_date = 2026-01-01`: a fabricated holding period if a caller forgets `with_entry_date`.
- `match_lots` never reports the unmatched remainder ("the caller's concern"), so shorting/flip fills lose quantity silently.

## Why

Wrong tax-bucket totals in the tax summary; intraday trades consume delivery cost basis. The sentinel date is a hardcoded stand-in that violates the no-silent-fake-defaults rule (`.agents/rules/interface-first-and-modularity.md` §1.3).

## How to close it

1. Add `product: Product` and a `holding_threshold_days` (or `AssetClass`) to `TaxLot`; match product-aware (same product first).
2. Inject an exchange-local date (`NaiveDate`, IST) via `LotFill.trade_date`.
3. Return `MatchResult { matched, unmatched_qty }` from `match_lots`.
4. Rebuild `TaxSummary` from `MatchedLot`s.
5. Remove the sentinel default: make `entry_date` a required constructor argument.

**Verification**: golden STCG/LTCG fixtures across the IST midnight boundary; proptest `sum(matched) + unmatched == fill qty`. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p tantra-ledger-portfolio --profile prod`, `make prod`.

## Related

- Root gap 0020 (time semantics); [0015](0015-apply-fill-ignores-side.md); plan 0006 R3 (u64 quantity and T+1 date from UTC); root plan 0019 P3.1/P3.3 (structure only).
