//! `parity` — the accounting/equity parity suites, r5 golden fixtures included: ONE test binary
//! over what used to be six. Same shape and same eligibility rule as `tests/recon.rs` beside this
//! file — grouped per `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section; test
//! names and bodies unchanged, only the `--test <binary>` slot (`--test r5_parity` is now
//! `--test parity`). Fixture paths are `CARGO_MANIFEST_DIR`-anchored, so the move is inert.

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
