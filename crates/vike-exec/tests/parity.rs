//! `parity` — the accounting/equity parity suites, r5 golden fixtures included, as ONE test
//! binary: same shape and eligibility rule as `tests/recon.rs`, grouped per
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section. Fixture paths are
//! `CARGO_MANIFEST_DIR`-anchored, so a member's location does not matter.

// The shared `EngineBuilder`, one copy for every group root.
#[path = "support/mod.rs"]
mod support;

// `#[path]` because this file is a test-target CRATE ROOT (see `tests/recon.rs`).
#[path = "parity/account_multi_asset.rs"]
mod account_multi_asset;
#[path = "parity/account_parity.rs"]
mod account_parity;
#[path = "parity/cross_venue_equity_parity.rs"]
mod cross_venue_equity_parity;
#[path = "parity/portfolio_robustness.rs"]
mod portfolio_robustness;
#[path = "parity/r5_parity.rs"]
mod r5_parity;
#[path = "parity/resolve_equity.rs"]
mod resolve_equity;
