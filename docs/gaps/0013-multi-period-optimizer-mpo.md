# Gap 0013 — Multi-Period Optimizer (MPO) Not Implemented

Severity: Medium · Status: Open · Tracked by: `tantra-agent/docs/plans/0007` Track 4 Phase 11

## What's Missing

`tantra-ledger-allocation` implements single-period objectives (`MeanVariance`,
`RiskParity`, `Hrp`, `BlackLitterman`) but no multi-period rebalancing optimizer.
Plan 0007 Track 4 Phase 11 scopes `tantra_multi_period_optimize`: a dynamic
portfolio rebalancing sequence over `T` periods that incorporates quadratic
transaction costs and a linear turnover penalty, then the MCP gateway tool.

## Requirements

1. New module `crates/tantra-ledger-allocation/src/mpo.rs` (pure Decimal,
   zero-I/O, deterministic — matching the crate's existing discipline).
2. Public API:
   - `MpoConfig { periods, transaction_cost, turnover_penalty, max_iterations }`
     (exact `Decimal` coefficients).
   - `MpoStep { period, target_weights, turnover, transaction_cost_incurred }`.
   - `MpoResult { steps, total_transaction_cost, total_turnover }`.
   - `multi_period_optimize(objective, constraints, state, config) -> Result<MpoResult, AllocationError>`.
3. Reuse the crate's box-constrained single-period solver per period and blend
   consecutive targets under the turnover/cost budget (documented deterministic
   algorithm).
4. Tests in `crates/tantra-ledger-allocation/tests/` (TDD, no inline `#[test]`).
5. MCP tool `tantra_multi_period_optimize` registered in `tantra-agent-mcp` once
   this gap is closed.

## Why It Matters

Portfolio rebalancing with costs and turnover limits is the realistic
institutional use case; a naive per-period re-optimization churns positions and
wastes on transaction costs.
