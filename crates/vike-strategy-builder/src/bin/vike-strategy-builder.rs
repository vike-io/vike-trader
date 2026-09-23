//! `vike-strategy-builder` — the ONLY process in this workspace that compiles a user-supplied
//! Rust strategy. Task 6 (Track B); see
//! `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md` ("The builder
//! service") for the design this binary implements.
//!
//! ⚠ This crate (library + this binary) moved out of `vike-strategy-plugin` by a controller
//! ruling — see `crates/vike-strategy-builder/src/builder.rs`'s module doc for why. Nothing about
//! what this binary itself does changed.
//!
//! ⚠ This file is deliberately THIN — every testable decision (the auth domain, the scope
//! ceiling, the wire handshake, retention) lives in `vike_strategy_builder::builder`, because a
//! `tests/*.rs` integration test can only link this crate's LIBRARY target, never a `src/bin/*.rs`
//! binary. See that module's doc for the full argument.
//!
//! ⚠ **This binary used to also own every `VIKE_STRATEGY_BUILDER_*` refusal and default, and that
//! stopped being true when `vike-backend strategy-builder` became this service's SECOND
//! composition root.** `builder::run` now carries the whole sequence — see its own doc for why a
//! second copy of every refusal message was the alternative. This file does exactly what a second
//! composition root cannot share: read the real process environment ONCE and hand the map to
//! [`vike_strategy_builder::builder::run`].
//!
//! ⚠ **Scope note, left here rather than silently omitted.** This binary does NOT route through
//! `vike-boot`'s shared startup sequence (`crates/vike-boot`) the way every other composition root
//! in this tree is expected to — no `<project>/settings` walk, no `policy.toml`/`config.toml`, no
//! credential-store read, no `vike-log` subscriber. It reads exactly the small, self-contained set
//! of `VIKE_STRATEGY_BUILDER_*` variables `builder::run` names, directly from the process
//! environment this binary owns. That is a genuine gap against this workspace's "one owner for the
//! startup sequence" rule (`crates/vike-boot/tests/one_owner.rs`), left open deliberately rather
//! than folded in silently: wiring the credential store, `policy.toml` and `vike-log` init is real
//! additional surface (new `vike-config`/`vike-ops` registry rows, a `deploy/` unit review) that
//! Task 6's brief does not ask for and that deserves its own review rather than arriving as a
//! rider on this one. Track B's report flags it explicitly.

use std::collections::HashMap;
use std::process::ExitCode;

fn main() -> ExitCode {
    // The ONE `std::env::vars()` sweep this binary performs — `builder::run` and everything below
    // it is a map lookup over this, never a second direct `std::env::var` call, so this stays the
    // single place that reads the real process environment.
    let vars: HashMap<String, String> = std::env::vars().collect();
    vike_strategy_builder::builder::run(&vars)
}
