//! **vike-aster's top-level test files as ONE test binary** (`cargo test -p vike-aster --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, and its `//!` contract; here it is a MODULE, so a test's id is
//! `<name>::<test>` and `cargo test -p vike-aster --test integration -- <name>::` runs one file (a live smoke adds
//! `--ignored --nocapture`, as its own `//!` says: all but `aster_market_data_smoke` can reach MAINNET with LIVE
//! keys). One binary is compiled and linked instead of one per file, by the rule of `crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation". Each member's own auto-discovered target is switched off in this crate's
//! `Cargo.toml` (`test = false`), which says why discovery stays on here.
//!
//! Still their own binaries: `offline`, the plain offline group (its module doc carries what may join it), and
//! `testnet_claim_gate` (the public mirror deletes it, `scripts/publish_mirror.sh`'s DENY).

mod aster_market_data_smoke;
mod aster_reconcile_smoke;
mod aster_smoke;
mod aster_userdata_soak;
mod decoder_never_panics;
mod order_client_id_map_failure_repro;
