# `tantra-ledger` — Implementation Record

Dismantled from the former `docs/roadmap/` (`roadmap.md` +
`migration-verification.md`). Completed milestones are recorded here; remaining
future work moved to [`docs/gaps/`](../gaps/README.md) and
[`docs/plans/`](../plans/README.md).

## Migration: `honba-kokoro` → `tantra-ledger` (Tier 1B)

| Milestone | Status | Commit |
|-----------|--------|--------|
| Workspace manifest purity (no tokio, canonical path deps) | Complete | `e22c95f` |
| OMS pure synchronous state machine | Complete | `08112a8` |
| Portfolio FIFO tax-lot on `AccountState` aggregate | Complete | `7c56840` |
| Journal double-entry ledger + `AccountCode` | Complete | `2019ae6` |
| Facade re-exports + repository traits + sizing | Complete | `5b6a051` |

The file-by-file mapping is in [`code-migration-map.md`](code-migration-map.md).

## Subsequent landings

| Crate | Landing | Commit(s) |
|-------|---------|-----------|
| `tantra-ledger-allocation` (mean-variance + risk parity, Phase 1) | Complete | `b35a314`, `436a2d8`, `53de244` |
| `tantra-ledger-performance` (annotations, metrics, calendar, compare) | Complete | `b7d7fec`, `2a3a04e`, `1b7fb97` |
| `tantra-ledger-allocation` plan 0003 (Hrp, Black-Litterman, exact bounds, discrete allocation) | Complete | this change |
| `tantra-ledger-performance` plan 0004 (deterministic `MetricsInput`, edge score v2) | Complete | this change |
| Docs: per-crate code references + implementation record | Complete | this change |

## Verification (2026-11-13, `--profile prod`)

```bash
grep -rn "#\[cfg(test)\]" crates/*/src/   # zero inline tests in production source
grep -rn "Utc::now" crates/*/src/         # zero wall-clock queries in production source
cargo fmt --check
cargo clippy --workspace --all-targets --profile prod -- -D warnings
cargo test --workspace --profile prod     # 200 tests, all passing
```

- Zero inline `#[cfg(test)]` blocks in `crates/*/src/` (all tests live in
  `tests/` parallel to `src/`).
- Zero `Utc::now()` in `crates/*/src/` (plan 0004 removed the last one — the
  only wall-clock query in the whole workspace's domain sources).
- Test distribution: facade 4, OMS 25, portfolio 36 (23 + 7 + 6), journal 58
  (18 + 27 + 8 + 5, incl. proptest invariants), allocation 32, performance 45
  (10 + 10 + 15 + 6 + 4). Total **200**.

## Open future work

All open items now live in [`docs/gaps/`](../gaps/README.md) and
[`docs/plans/`](../plans/README.md) — the remaining settlement &
statutory-fee consolidation plan 0005: external NSE calendar, per-lot
corporate actions, per-segment fee tables, canonical golden framework, and
OMS context wiring.
