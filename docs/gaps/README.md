# Gap Index — `tantra-ledger`

Per workspace rule `.agents/rules/gap-tracking.md`:
- **Gaps** (`docs/gaps/XXXX-*.md`): individual missing features, stubs, or rule violations.
- **Plans** (`docs/plans/XXXX-*.md`): collections of gaps.
- Numbering is per-repo, sequential, never reused.

Dismantled from the former `docs/roadmap/` directory: every completed milestone
now lives in [`docs/design/`](../design/architecture.md) +
[`docs/design/implementation-record.md`](../design/implementation-record.md);
the remaining future work is enumerated below.

## Open Gaps

| # | Title | Severity | Status | Plan |
|---|-------|----------|--------|------|
| [0007](0007-portfolio-hardcoded-nse-calendar.md) | Portfolio `ClearingCalendar::new()` hardcodes NSE holidays 2024–2026 | Medium | Open | [0005](../plans/0005-settlement-and-fee-consolidation.md) |
| [0008](0008-portfolio-corporate-action-per-lot-cost-basis.md) | Corporate-action adjustments are aggregate-level, not per-lot | Medium | Open | [0005](../plans/0005-settlement-and-fee-consolidation.md) |
| [0009](0009-journal-per-segment-fee-tables.md) | Journal exchange turnover fee tables fall back to NSE cash-segment rate for NFO/MCX/CDS | Medium | Open | [0005](../plans/0005-settlement-and-fee-consolidation.md) |
| [0010](0010-journal-golden-framework-legacy-fees.md) | Golden contract-note framework runs on the legacy floor-rounded fee track | Low | Open | [0005](../plans/0005-settlement-and-fee-consolidation.md) |
| [0011](0011-oms-context-types-unwired.md) | OMS context types (`OmsConfig`, `MarketData`, `PortfolioRiskState`) defined but not wired into validation | Medium | Open | [0005](../plans/0005-settlement-and-fee-consolidation.md) |

## Closed Gaps

| # | Title | Closed In |
|---|-------|-----------|
| 0001 | Allocation `Hrp` objective declared but not implemented | plan 0003 — HRP solver (correlation-distance single linkage, quasi-diagonalization, recursive bisection), `2026-11-13` |
| 0002 | Allocation `BlackLitterman` objective declared but not implemented | plan 0003 — `BlackLittermanViews`/`View` inputs + posterior blend, `2026-11-13` |
| 0003 | Allocation bound handling is clip-and-renormalize, not exact bound-constrained QP | plan 0003 — `project_box_simplex` + projected Sharpe / projected fixed point / HRP projection, `2026-11-13` |
| 0004 | Discrete / whole-share allocation not implemented | plan 0003 — `allocate_discrete` (iterative largest-remainder), `2026-11-13` |
| 0005 | Performance `MetricsInput::new()` mints `as_of` via `Utc::now()` | plan 0004 — `as_of` required constructor arg; runtime call sites updated, `2026-11-13` |
| 0006 | Performance edge score is a v1 placeholder formula | plan 0004 — v2 weighted spec (`EDGE_SCORE_VERSION = 2`), `2026-11-13` |
