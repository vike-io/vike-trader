//! `risk` — the RiskGate suites (lane completion, modify-path gating, margin, symbol coherence) as
//! ONE test binary: same shape and eligibility rule as `tests/recon.rs`, grouped per
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section.

// The shared `EngineBuilder`, one copy for every group root.
#[path = "support/mod.rs"]
mod support;
// `#[path]` because this file is a test-target CRATE ROOT (see `tests/recon.rs`).
#[path = "risk/gate_symbol_coherence.rs"]
mod gate_symbol_coherence;
#[path = "risk/risk_account_ceiling.rs"]
mod risk_account_ceiling;
#[path = "risk/risk_gate_on_modify.rs"]
mod risk_gate_on_modify;
#[path = "risk/risk_lane_common.rs"]
mod risk_lane_common;
#[path = "risk/risk_lane_coverage.rs"]
mod risk_lane_coverage;
#[path = "risk/risk_lane_pricing.rs"]
mod risk_lane_pricing;
#[path = "risk/risk_margin.rs"]
mod risk_margin;
