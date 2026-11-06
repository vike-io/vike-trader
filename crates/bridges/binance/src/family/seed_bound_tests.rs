use super::*;
use vike_bridge_core::pump_spec::CONNECT_10S;

/// **The depth book SEED is bounded, and bounded to fit the dial** — the test this defect needed
/// and did not have.
///
/// The dial-bounding work stopped at the dial, and a depth session is dial → seed → read loop.
/// The seed ran on the shared `vike_bridge_core::http::blocking_agent`, a **30 s** global timeout
/// sized for a backfill pager, so the longest window a feed thread could sit in at `systemctl
/// stop` was never the newly-bounded 10 s dial: it was a 30 s HTTP call immediately behind it,
/// absent from every derivation and from `crates/vike-datahub/src/recorder.rs`'s
/// `FEED_STOP_BUDGET_SECS`. Exactly the shape `crates/bridges/binance/src/family/trades.rs`'s
/// `the_startup_warmup_runs_on_a_bounded_agent_not_the_pagers` caught one lane over.
///
/// Three assertions, each failing on a different way of reopening it: the seed agent really
/// carries [`DEPTH_SEED_TIMEOUT`] (revert to `blocking_agent()` and this goes red); that window is
/// genuinely SHORTER than the shared default (so the test cannot be satisfied by the two
/// converging on 30 s); and it fits inside the dial window the recorder's budget is derived from
/// (so a future widening has to move that budget deliberately rather than silently).
///
/// MUTATION PROOF: point [`depth_seed_agent`] back at
/// `vike_bridge_core::http::blocking_agent()` — assertion one fails on the value, assertion two
/// on the ordering. Reads only agent configuration, so it fails identically on any box.
#[test]
fn the_depth_seed_runs_on_a_bounded_agent_not_the_pagers() {
    let seed = depth_seed_agent().config().timeouts().global;
    assert_eq!(
        seed,
        Some(DEPTH_SEED_TIMEOUT),
        "the depth book seed must run on its own bounded agent — it is the last blocking call \
             between a depth session's dial and its first stop poll"
    );

    let shared = vike_bridge_core::http::blocking_agent()
        .config()
        .timeouts()
        .global
        .expect("the shared agent has always carried a global timeout");
    assert!(
        DEPTH_SEED_TIMEOUT < shared,
        "the seed bound ({DEPTH_SEED_TIMEOUT:?}) must be shorter than the shared pager agent's \
             ({shared:?}), or this test is satisfied by the defect"
    );

    assert!(
        DEPTH_SEED_TIMEOUT <= CONNECT_10S,
        "the depth seed ({DEPTH_SEED_TIMEOUT:?}) is now the LARGEST window a feed thread can be \
             caught in, bigger than the dial ({CONNECT_10S:?}) the recorder's FEED_STOP_BUDGET_SECS \
             is derived from. Shorten it, or re-derive that budget deliberately."
    );
}
