//! vike-market-making's MATHS tests that live outside their module, one sibling file per family (audit F11),
//! and the helpers more than one test module reads. The MAKER's tests (every test that drove a
//! `SpreadMaker` or read its fields) moved with the maker to its strategy file's own test module,
//! `strategies/market_making/spread_maker.rs` (plan 2b); the families here keep the pure-function
//! half of the files they shared with it.
//!
//! This module is a DESCENDANT of the crate root, so the `pub(crate)` maths (`avellaneda`'s
//! `as_quotes`, `fits`' `fit_kappa`, …) stay reachable exactly as they were.

use super::*;
// The pure helpers these tests exercise live in sibling modules.
use crate::avellaneda::*;
use crate::fits::*;
use crate::skew::*;
use std::collections::VecDeque;
// The shared L1-quote builder (sizes 1.0, empty symbol) — the families reach it through their
// `use super::*`.
use vike_marketdata::test_support::quote;
use vike_model::params::{ParamValue, Params};
use vike_model::{AsParams, HorizonMode, KappaMode, QuoteStyle, VarianceMode};

mod as_pricing;
mod breaker;
mod from_params;
mod quote_style;
/// The hint rows for the old crate name (G14).
mod renamed_names;
mod skew_sizing;

/// Capture every `tracing` event emitted on this thread while `f` runs, as text — one line per
/// event, `LEVEL target: message field=value …`, so a test can ask whether a sentence or a field
/// was said and how many times.
///
/// ⚠ **The only way to test the thing the maker's HOLD warns are FOR.** Both
/// `AsState::note_fee_floor` and the spread maker's `note_no_quote` exist so a hold is SAID; a test
/// that asserts only the `None` return passes with the `warn!` deleted, which is the
/// declaration-pinning shape this repo has been bitten by three times — it would pin the mechanism
/// against a copy of itself and prove nothing about what an operator sees.
/// `avellaneda::fee_floor_tests` reads this copy; the spread maker file's tests
/// (`strategies/market_making/spread_maker.rs`) carry the one their `silent_hold` reads.
///
/// ⚠ It reads through `vike_log::capture::captured`, the workspace's one scoped capture, and is a
/// rendering of what that returns. It used to install a `tracing-subscriber` formatter of its own
/// with a bare `with_default`, and that could come back EMPTY under `cargo test`: the HOLD lines
/// are also reached UNCAPTURED by sibling tests here (`the_prod2_bybit_mount_posts_nothing_…` is
/// one), and while one capture is the only live dispatcher, the first thread to reach a callsite
/// decides its cached interest for every thread. The shared capture's module doc carries the
/// mechanism and the anchor that closes it.
pub(crate) fn captured_logs(f: impl FnOnce()) -> String {
    let ((), events) = vike_log::capture::captured(f);
    let mut text = String::new();
    for e in &events {
        text.push_str(&format!("{} {}: {}", e.level, e.target, e.message));
        for (k, v) in &e.fields {
            text.push_str(&format!(" {k}={v}"));
        }
        text.push('\n');
    }
    text
}

/// A-S params anchored on a 300 s time-to-resolution window closing at T = 300_000 ms, with the
/// underlying blend at `weight` (a `0.0` weight = the blend OFF control). Blackout off + fixed κ, so
/// the ONLY thing that moves between two makers built from it is the underlying anchor. Read by
/// `avellaneda`'s `underlying_anchor_tests` (the spread maker file's tests carry a copy).
pub(crate) fn blend_params(weight: f64) -> AsParams {
    AsParams {
        underlying_weight: weight,
        underlying_beta: 1.0,
        window_secs: 300.0,
        horizon_mode: HorizonMode::TimeToResolution,
        resolution_ts: Some(300_000),
        resolution_blackout_ms: 0,
        gamma: 0.5,
        kappa_mode: KappaMode::Fixed,
        q_scale: 1.0,
        min_standoff_ticks: 1.0,
        ..AsParams::default()
    }
}

/// The the CI box grid: bybit BTCUSDT on its real `0.1` tick (`SpreadMakerPreset::Crypto`'s knobs), read
/// by `avellaneda::fee_floor_tests` (the spread maker file's tests carry a copy).
pub(crate) const TICK: f64 = 0.1;

pub(crate) const MID: f64 = 63_050.0;

/// A healthy two-sided view — one level a side, straddling [`MID`].
pub(crate) fn two_sided() -> BookView {
    BookView {
        tick_size: TICK,
        bids: vec![vike_marketdata::L2BookLevel::new(MID - 0.5 * TICK, 5.0)],
        asks: vec![vike_marketdata::L2BookLevel::new(MID + 0.5 * TICK, 5.0)],
    }
}
