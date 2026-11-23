# 0018 — Double-entry journal: runtime-only balance checks, public mutable state, no tx-id idempotency

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: high
**Plan**: [0006](../plans/0006-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-ledger-journal/src/lib.rs:125-142` — `Transaction` has all-`pub` fields; `post_transaction` is a free function that returns `Ok(())` after a sum check and posts nothing. A transaction with both maps empty is "balanced" (0 == 0) in `DoubleEntryLedger::post` (`:164-190`).
- `crates/tantra-ledger-journal/src/lib.rs:150-154` — `DoubleEntryLedger { pub debit_balances, pub credit_balances, pub transactions }`: any caller can mutate balances without a transaction. `LedgerError::DuplicateEntry` / `InsufficientBalance` / `UnknownAccount` (`:54-62`) are declared but never constructed, so a duplicate `tx_id` posts twice.
- `crates/tantra-ledger-journal/src/lib.rs:75` — `AccountCode(pub String)` is free-form; the chart of accounts is string constants with no account type (Asset/Liability/Equity/Income/Expense); sign convention lives in callers. `record_buy` hard-codes `HOLDINGS_EQUITY_DELIVERY`, so intraday/F&O fills are booked as delivery holdings (`:314-380`).
- Amounts are bare `Decimal` (no `Currency`): currency mixing is prevented by convention only. `record_sell` with `cost_basis == 0` (e.g. short-sale open) credits a zero leg and fails `InvalidAmount` (`:~455`).
- Contrast: `crates/tantra-ledger/src/repository.rs:136-175` `JournalTransaction::validate` is stricter (min 1 debit + 1 credit, `Money` legs) but is a second, parallel type (plan 0006 R1).

## Why

Duplicate postings (retry after timeout) double-count cash and P&L; a stray `ledger.debit_balances.insert(..)` breaks the accounting identity with no trace; F&O trades cannot be journaled correctly. Violates the DDD rule on rich aggregates (`.agents/rules/coding-and-ddd.md` §1.2).

## How to close it

1. Introduce `PostedTransaction`, produced only by `Transaction::new(..) -> Result` (>= 2 legs, each `Money` in one currency, positive, balanced); make `DoubleEntryLedger` fields private.
2. Enforce unique `TransactionId` and return `DuplicateEntry`.
3. Add `AccountKind` to `AccountCode`/the chart, with typed constructors.
4. Keep balances as a projection of the append-only transaction log (rebuilt in `from_log`).
5. Parametrize `record_buy`/`record_sell` by product/segment account; allow zero-cost-basis short opens.

**Verification**: proptest "any accepted tx sequence keeps total_debits == total_credits"; duplicate-id test; replay-from-log equality. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p tantra-ledger-journal --profile prod`, `make prod`.

## Related

- [0019](0019-event-log-and-snapshots.md) (`LedgerEvent` log); plan 0006 R1 (duplicate ledger models); root plan 0019 P4.1 covers cost tables only.
