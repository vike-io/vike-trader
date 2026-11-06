use super::*;
use crate::market_feed::BINANCE_URLS;
use vike_bridge_core::pump_spec::market_pump_spec;

/// The production [`UrlTable`] with ONE field changed: a stream host that cannot be parsed as a
/// URL at all, so a dial through it fails BEFORE any socket, DNS lookup or network exists —
/// the whole point, since the property under test is which code path did the failing, not
/// whether a host is reachable.
///
/// The space is load-bearing: it is what makes the `http::Uri` parse fail deterministically on
/// every platform. `crates/vike-bridge-core/src/ws_proxy_tests.rs`'s
/// `no_proxy_bounded_arm_fails_in_the_url_parse_exactly_as_before` already pins the same input
/// against the same parse.
fn unparseable_urls() -> UrlTable {
    UrlTable { spot_ws: "not a url", ..BINANCE_URLS }
}

/// **The dial this lane performs is BOUNDED, and the bound is its `MarketPumpSpec` row's.**
///
/// This is the test the defect needed and did not have. `connect_trades_ws` owned its own
/// connect (the startup drain needs the raw socket), dialed with plain `tungstenite::connect`,
/// and therefore applied no connect bound at all — while the venue's row ALSO said
/// `connect_timeout: None`, so neither half of the answer was in place and every existing test
/// passed. A recorder stopping while that socket was mid-dial ignored the stop flag for the OS's
/// SYN ladder, blowing `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`.
///
/// **How it observes the path without a network.** `vike_bridge_core::ws_proxy::connect_ws`
/// branches on `connect_timeout`: `Some` parses the TCP target itself first (`ws_target`, whose
/// rejection is the distinctive `"bad ws url"`), `None` hands the whole string to
/// `tungstenite::connect`, which fails in ITS parser with its own wording. So an unparseable
/// host makes the two arms say different things, offline and deterministically — and the second
/// half of this test asserts they really do differ, so the discriminator cannot quietly become
/// something both arms satisfy.
///
/// Both ways of reopening the defect go red here: put `connect_timeout: None` back in the
/// binance row, or dial with `tungstenite::connect` again, and the error is no longer the
/// bounded arm's.
#[test]
fn the_trades_dial_goes_through_the_bounded_shared_path() {
    let opts = pump_opts("binance");
    assert_eq!(
        opts.connect_timeout,
        market_pump_spec("binance").knobs().connect_timeout,
        "the lane's opts must BE the row, not a local copy of it"
    );
    assert!(
        opts.connect_timeout.is_some(),
        "binance's row must bound the dial — see pump_spec's every_on_driver_row_bounds_its_dial"
    );

    let bounded = connect_trades_ws(&unparseable_urls(), "BTCUSDT", false, &opts)
        .expect_err("an unparseable host cannot dial");
    assert!(
        bounded.starts_with("bad ws url"),
        "the trades dial did not take `connect_ws`'s BOUNDED arm — it either ignored its row's \
             connect_timeout (a hand-rolled `tungstenite::connect`) or the row lost its bound. \
             error was: {bounded}"
    );

    // …and the discriminator discriminates: the same call with the bound removed fails
    // somewhere else entirely. Without this, a future edit could make BOTH arms produce the
    // asserted prefix and the check above would pass through the defect it exists to catch.
    let unbounded_opts = MarketPumpOpts { connect_timeout: None, ..opts };
    let unbounded = connect_trades_ws(&unparseable_urls(), "BTCUSDT", false, &unbounded_opts)
        .expect_err("an unparseable host cannot dial");
    assert!(
        !unbounded.starts_with("bad ws url"),
        "the unbounded arm must be distinguishable from the bounded one, or this test proves \
             nothing. error was: {unbounded}"
    );
}

/// The drain's wall-clock ceiling and the socket's read timeout are ONE span stated twice (see
/// [`DRAIN_DEADLINE`]) — pinned equal here because the drain's contract ("never blocks past one
/// read tick") is a claim about the socket, and the socket's timeout now comes from the row.
#[test]
fn the_drain_deadline_matches_the_rows_read_timeout() {
    assert_eq!(DRAIN_DEADLINE, market_pump_spec("binance").knobs().read_timeout);
    assert_eq!(DRAIN_DEADLINE, market_pump_spec("aster").knobs().read_timeout);
}

/// **The startup REST warmup is bounded, and bounded to fit the dial.**
///
/// Bounding the dial is not the same as bounding the connect closure, and this is where that
/// gap lived: [`run_trades_feed`]'s closure calls [`fetch_agg_trades_latest`] between the dial
/// and the drain, and it did so on the shared `vike_bridge_core::http::blocking_agent` — a
/// **30 s** global timeout sized for a backfill pager. So the longest window a feed thread could
/// be caught in at `systemctl stop` was not the newly-bounded 10 s dial at all; it was a 30 s
/// HTTP call two lines below it, absent from every derivation and from
/// `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`.
///
/// Three assertions, each of which fails on a different way of reopening it: the warmup agent
/// really carries [`WARMUP_TIMEOUT`] (revert to `blocking_agent()` and this goes red), that
/// window is genuinely SHORTER than the shared default (so the test cannot be satisfied by the
/// two converging on 30 s), and it fits inside the dial window the recorder's budget is derived
/// from (so a future widening has to move the budget deliberately rather than silently).
#[test]
fn the_startup_warmup_runs_on_a_bounded_agent_not_the_pagers() {
    let warmup = warmup_agent().config().timeouts().global;
    assert_eq!(
        warmup,
        Some(WARMUP_TIMEOUT),
        "the startup warmup must run on its own bounded agent — it sits inside the connect \
             closure a live recorder's teardown waits on"
    );

    let shared = vike_bridge_core::http::blocking_agent()
        .config()
        .timeouts()
        .global
        .expect("the shared agent has always carried a global timeout");
    assert!(
        WARMUP_TIMEOUT < shared,
        "the warmup bound ({WARMUP_TIMEOUT:?}) must be shorter than the shared pager agent's \
             ({shared:?}), or this test is satisfied by the defect"
    );

    let dial = market_pump_spec("binance")
        .knobs()
        .connect_timeout
        .expect("binance's row bounds its dial — see every_on_driver_row_bounds_its_dial");
    assert!(
        WARMUP_TIMEOUT <= dial,
        "the warmup ({WARMUP_TIMEOUT:?}) is now the LARGEST window a feed thread can be caught \
             in, bigger than the dial ({dial:?}) the recorder's FEED_STOP_BUDGET_SECS is derived \
             from. Shorten it, or re-derive that budget deliberately."
    );
}
