//! `risk` — the RiskGate suites (lane completion, modify-path gating, margin, symbol coherence):
//! ONE test binary over what used to be four. Same shape and same eligibility rule as
//! `tests/recon.rs` beside this file — grouped per `crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation" section; test names and bodies unchanged, only the
//! `--test <binary>` slot.

// `#[path]` because this file is a test-target CRATE ROOT (see `tests/recon.rs`).
#[path = "risk/gate_symbol_coherence.rs"]
mod gate_symbol_coherence;
#[path = "risk/risk_gate_on_modify.rs"]
mod risk_gate_on_modify;
#[path = "risk/risk_lane_completion.rs"]
mod risk_lane_completion;
#[path = "risk/risk_margin.rs"]
mod risk_margin;
