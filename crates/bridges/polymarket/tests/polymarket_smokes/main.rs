//! **The live Polymarket smokes, ONE test binary** (`--test polymarket_smokes`): seven `#[ignore]`d
//! files that each opened the egress helper by a path attribute, so it compiled seven times. Grouped,
//! `egress_support` compiles once. Each member's own module doc stays the authority for what it
//! touches (keyless read, authenticated read, real money). Run ONE member at a time, as
//! `cargo test -p vike-polymarket --features polymarket --test polymarket_smokes -- --ignored --nocapture <member>::`:
//! the egress is a process-wide cell, so two members declaring different routes in one `cargo test`
//! process would race for it (nextest gives each test its own process).

// Every member shares this one gate: the whole library is behind the non-default `polymarket`
// feature (audit G2), so default-feature `cargo test` must compile nothing here.
#![cfg(feature = "polymarket")]

mod chain_settlement_smoke;
mod egress_support;
mod gamma_smoke;
mod market_ticks_live_smoke;
mod poly_exec_mount_smoke;
mod polymarket_reconcile_smoke;
mod polymarket_user_smoke;
mod redeem_smoke;
