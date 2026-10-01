//! `offline` -- vike-polymarket's plain offline suite: ONE test binary over what used to be six.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each former
//! `tests/<name>.rs` member here is now `tests/offline/<name>.rs`, included below as a plain
//! module: the tests, their names, their bodies and their scripted fixtures are byte-unchanged --
//! only the binary they link into moved.
//!
//! ```sh
//! cargo test -p vike-polymarket --features polymarket --test offline                    # the whole group
//! cargo test -p vike-polymarket --features polymarket --test offline -- rtds_scripted  # one member
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s, with the one sanctioned bridge
//! extension: the `#![cfg(feature = "polymarket")]` below is a GROUP-level gate (the FULL-venue
//! feature, not the crate-level `feeds` one),
//! legal because every member carried this IDENTICAL gate as a standalone binary -- it is hoisted
//! here once and the members carry none of their own. A default (feature-less) build compiles
//! this binary to an empty crate and never reads the member files, exactly as it compiled each
//! standalone binary to an empty crate before. The `#[ignore]`d live smokes stay their own
//! binaries -- because they are LIVE and separately gated, never run casually. ⚠ Their exposure
//! VARIES and this line called them all "real-money mainnet" until 2026-08-18: `gamma_smoke.rs`,
//! `market_ticks_live_smoke.rs` and `chain_settlement_smoke.rs` are KEYLESS and READ-ONLY, while
//! `redeem_smoke.rs` genuinely transacts. Each file's own module doc is the authority; see this
//! crate's `CLAUDE.md`.
#![cfg(feature = "polymarket")]

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod rtds_scripted;` would
// resolve against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/dust_snap_engine.rs"]
mod dust_snap_engine;
#[path = "offline/market_feed_scripted.rs"]
mod market_feed_scripted;
#[path = "offline/market_feed_shard.rs"]
mod market_feed_shard;
#[path = "offline/polymarket_reconcile_parse.rs"]
mod polymarket_reconcile_parse;
#[path = "offline/resolve_settlement.rs"]
mod resolve_settlement;
#[path = "offline/resync_history_floor.rs"]
mod resync_history_floor;
#[path = "offline/rtds_scripted.rs"]
mod rtds_scripted;
