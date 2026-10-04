//! `SpreadMaker`'s unit tests, split out of the crate root into one sibling file per feature
//! family (audit F11 — they were ~63% of a 3.8k-line `lib.rs`). Every test moved VERBATIM; this
//! module owns only what more than one family needs: the shared imports and the `LiveBroker`
//! fixture helpers. A per-family helper (`ofi_tox_params`, `all_new_params`, `MockHft`,
//! `approx_rel`, `mid_ladder`, `reward_bag`, …) stays in the file that uses it.
//!
//! Privacy note: this module is a DESCENDANT of the crate root, so `SpreadMaker`'s private fields
//! (`cfg`/`bid`/`ask`/`fills`/`as_state`/`last_flow`) and private methods (`breaker_enabled`,
//! `pull_accel_mult`) stay reachable exactly as they were inside the old inline `mod tests`.

use super::*;
// The pure helpers these tests exercise live in sibling modules (byte-identical move).
use crate::avellaneda::*;
use crate::book::*;
use crate::fits::*;
use crate::skew::*;
// vike-model types the tests use directly (previously reached via the crate-root re-export,
// trimmed when the logic moved to sibling modules).
use vike_model::{
    Fill, HftBroker, HorizonMode, KappaMode, L2Book, LadderOffsetUnit, LadderSizeProfile,
    PriceDomain, QuoteTick, Strategy, StrategyParams, TradeTick, VarianceMode,
};
// The concrete live broker the existing tests build directly; it `impl HftBroker`, so the
// generic `SpreadMaker` mounts on it unchanged. `LiveBroker` lives in vike-core, pulled in as a
// TEST-ONLY dev-dependency (the vike-mm library depends on vike-model alone); the tests build it
// via a struct literal, which is why its buffered-order fields are `pub`.
use vike_core::LiveBroker;

mod as_pricing;
mod breaker;
mod flow_toxicity;
mod from_params;
mod ladder_quoting;
mod liquidity_rewards;
mod live_params;
mod quote_style;
mod refresh_tolerance;
/// The two edge-triggered HOLD warns, tested as BEHAVIOUR rather than as return values.
mod silent_hold;
mod skew_sizing;
// The CROSS-EXCHANGE maker's behaviour families — the same white-box `LiveBroker` rig as the
// `SpreadMaker` families above, on the crate's second reference strategy.
mod xemm_guards;
mod xemm_hedge;
mod xemm_mount;

/// Capture every `tracing` event emitted on this thread while `f` runs, as text — one line per
/// event, `LEVEL target: message field=value …`, so a test can ask whether a sentence or a field
/// was said and how many times.
///
/// ⚠ **The only way to test the thing the maker's HOLD warns are FOR.** Both
/// `AsState::note_fee_floor` and `SpreadMaker::note_no_quote` exist so a hold is SAID; a test that
/// asserts only the `None` return passes with the `warn!` deleted, which is the
/// declaration-pinning shape this repo has been bitten by three times — it would pin the mechanism
/// against a copy of itself and prove nothing about what an operator sees. Lives HERE, once, so the
/// two families that need it (`avellaneda::fee_floor_tests` and `tests::silent_hold`) share one
/// implementation rather than spelling it twice.
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

/// A bare in-crate `LiveBroker` at a given inventory + EVENT ts, empty order buffers — the
/// same struct the runtime builds per tick/fill, so what the strategy pushes into it is exactly
/// what the live drain would route.
fn broker(position: f64, now: i64) -> LiveBroker {
    LiveBroker {
        positions: Vec::new(),
        prices: Vec::new(),
        bar_views: Vec::new(),
        position,
        price: 0.0,
        equity: 0.0,
        bars: std::sync::Arc::new(Vec::new()),
        index: 0,
        now,
        multiplier: 1.0,
        lot_size: 0.0,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        brackets: Vec::new(),
        conditionals: Vec::new(),
        mass_cancel: false,
    }
}

fn quote(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: String::new() }
}

/// A fill at `side` (+1 bid/buy, −1 ask/sell), `size`, EVENT `ts`.
fn a_fill(side: i32, size: f64, ts: i64) -> Fill {
    Fill { side, size, price: 100.0, fee: 0.0, ts, is_maker: true, symbol: String::new() }
}

fn submitted(b: &LiveBroker, tag: &str) -> bool {
    b.submissions.iter().any(|s| s.tag.as_deref() == Some(tag))
}
fn modified(b: &LiveBroker, tag: &str) -> bool {
    b.modifications.iter().any(|m| m.tag == tag)
}
fn canceled(b: &LiveBroker, tag: &str) -> bool {
    b.cancels.iter().any(|t| t == tag)
}

/// `(price, qty)` of a tagged SUBMIT; `new_price` of a tagged MODIFY — the two reads the
/// style/filtration tests assert placement prices with.
fn submit_at(b: &LiveBroker, tag: &str) -> (f64, f64) {
    let s = b.submissions.iter().find(|s| s.tag.as_deref() == Some(tag)).expect("a submit");
    (s.price.expect("limit submit has a price"), s.qty)
}
fn modify_px(b: &LiveBroker, tag: &str) -> f64 {
    let m = b.modifications.iter().find(|m| m.tag == tag).expect("a modify");
    m.new_price.expect("re-price has a price")
}

/// One buffered submission, rendered — `tag`, `symbol`, `side`, `qty`, `price`.
///
/// `BufferedSubmit`/`BufferedModify` are runtime LANE PAYLOADS with no `Debug` impl, so an
/// assertion that fails on them can print nothing useful. The xemm families' failures are only
/// diagnosable with the whole row (which leg? tagged or not? what size?), so they render through
/// this instead.
type SubmitRow = (Option<String>, Option<String>, i32, f64, Option<f64>);

/// Every buffered submission as a [`SubmitRow`], for assertion messages.
fn submits_dbg(b: &LiveBroker) -> Vec<SubmitRow> {
    b.submissions
        .iter()
        .map(|s| (s.tag.clone(), s.symbol.clone(), s.side, s.qty, s.price))
        .collect()
}

/// The tags of every buffered modify — the `BufferedModify` twin of [`submits_dbg`].
fn modifies_dbg(b: &LiveBroker) -> Vec<String> {
    b.modifications.iter().map(|m| m.tag.clone()).collect()
}
