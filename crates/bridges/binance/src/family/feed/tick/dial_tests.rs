use std::sync::Arc;

use super::*;
use vike_bridge_core::pump_clock::VirtualClock;
use vike_bridge_core::scripted::{
    UserScriptHeard, UserScriptLog, UserScriptStep, WebsocketSessionUserStreamScript,
};
use vike_bridge_core::websocket_session::spec;
use vike_bridge_core::websocket_session::user::WebsocketSessionUserMessage;

/// The production [`UrlTable`] with ONE field changed: a stream host that cannot be parsed as a
/// URL at all, so a dial through it fails BEFORE any socket, DNS lookup or network exists —
/// the whole point, since the property under test is which code path did the failing, not
/// whether a host is reachable.
///
/// The space is load-bearing: it is what makes the `http::Uri` parse fail deterministically on
/// every platform. `crates/vike-transport/src/websocket_proxy_tests.rs`'s
/// `no_proxy_bounded_arm_fails_in_the_url_parse_exactly_as_before` already pins the same input
/// against the same parse.
fn unparseable_urls() -> UrlTable {
    UrlTable { spot_websocket: "not a url", ..crate::feed::seeded_tables().linear }
}

/// **The dial this lane performs is BOUNDED, and the bound is its pump row's (`spec::row`).**
///
/// This is the test the defect needed and did not have. `connect_trades_websocket` owned its own
/// connect (the startup drain needs the raw socket), dialed with plain `tungstenite::connect`,
/// and therefore applied no connect bound at all — while the venue's row ALSO said
/// `connect_timeout: None`, so neither half of the answer was in place and every existing test
/// passed. A recorder stopping while that socket was mid-dial ignored the stop flag for the OS's
/// SYN ladder, blowing `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`.
///
/// **How it observes the path without a network.** `vike_transport::websocket_proxy::connect_websocket`
/// branches on `connect_timeout`: `Some` parses the TCP target itself first (`websocket_target`, whose
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
    let opts = pump_opts("binance", None);
    assert_eq!(
        opts.connect_timeout,
        spec::row("binance").connect_timeout,
        "the lane's opts must BE the row, not a local copy of it"
    );
    assert!(
        opts.connect_timeout.is_some(),
        "binance's row must bound the dial — see pump_spec's every_on_driver_row_bounds_its_dial"
    );

    let bounded = connect_trades_websocket(
        &mut dial_trades_session,
        &unparseable_urls(),
        "BTCUSDT",
        false,
        &opts,
    )
    .err()
    .expect("an unparseable host cannot dial");
    assert!(
        bounded.starts_with("bad ws url"),
        "the trades dial did not take `connect_websocket`'s BOUNDED arm — it either ignored its row's \
             connect_timeout (a hand-rolled `tungstenite::connect`) or the row lost its bound. \
             error was: {bounded}"
    );

    // …and the discriminator discriminates: the same call with the bound removed fails
    // somewhere else entirely. Without this, a future edit could make BOTH arms produce the
    // asserted prefix and the check above would pass through the defect it exists to catch.
    let unbounded_opts = WebsocketSessionMarketOptions { connect_timeout: None, ..opts };
    let unbounded = connect_trades_websocket(
        &mut dial_trades_session,
        &unparseable_urls(),
        "BTCUSDT",
        false,
        &unbounded_opts,
    )
    .err()
    .expect("an unparseable host cannot dial");
    assert!(
        !unbounded.starts_with("bad ws url"),
        "the unbounded arm must be distinguishable from the bounded one, or this test proves \
             nothing. error was: {unbounded}"
    );
}

/// One `@aggTrade` push of BTCUSDT carrying the aggregate id `id`.
fn agg_trade(id: u64) -> String {
    format!(
        r#"{{"e":"aggTrade","E":1794160140001,"s":"BTCUSDT","a":{id},"p":"75640.0","q":"0.1","f":1,"l":1,"T":1794160140000,"m":false}}"#
    )
}

/// What the trade tape of `venue` did over its scripted sessions: when each dial came (ms of the virtual
/// clock), how often its REST warm-up ran, when it answered a server ping, and the frames it sent.
struct TapeRun {
    dials: Vec<u128>,
    warmups: usize,
    pongs: Vec<u128>,
    sent: Vec<String>,
}

/// Run `venue`'s trade tape (spot BTCUSDT, its family row over the seeded hosts) through
/// [`run_trades_feed_on`] over `sessions` (`None` a refused dial) on a fresh virtual clock; the last session
/// raises the lane's stop once drained.
fn tape_run(
    venue: &'static str,
    market_keepalive: Option<&'static str>,
    sessions: Vec<Option<Vec<UserScriptStep>>>,
) -> TapeRun {
    let clock = VirtualClock::new(1);
    let _seat = clock.enter();
    let mut sessions = std::collections::VecDeque::from(sessions);
    let stop = Arc::new(AtomicBool::new(false));
    let ctx = DataClientContext {
        venue,
        sink: Arc::new(vike_data::NoopSink),
        status: Arc::default(),
        wake: Arc::new(|| {}),
        stop: Arc::clone(&stop),
        own: (),
    };
    let spec = FamilySpec {
        venue,
        display: "Venue",
        urls: crate::feed::seeded_tables().linear,
        market_keepalive,
    };
    let log = UserScriptLog::default();
    let (mut dials, mut warmups) = (Vec::new(), 0);
    run_trades_feed_on(
        &spec,
        "BTCUSDT",
        "BTCUSDT",
        false,
        &ctx,
        &Mutex::default(),
        None,
        || {
            warmups += 1;
            Ok(Vec::new())
        },
        |_url, _opts| {
            dials.push(clock.offset(clock.now()).as_millis());
            let steps = sessions.pop_front().ok_or("a dial past the script")?.ok_or("refused")?;
            let session =
                WebsocketSessionUserStreamScript::from_steps(steps).logged(Arc::clone(&log));
            Ok(if sessions.is_empty() { session.stopping(Arc::clone(&stop)) } else { session })
        },
    );
    let (mut pongs, mut sent) = (Vec::new(), Vec::new());
    for (at, heard) in log.lock().unwrap().iter() {
        match heard {
            UserScriptHeard::Pong => pongs.push(clock.offset(*at).as_millis()),
            UserScriptHeard::Text(text) => sent.push(text.clone()),
        }
    }
    TapeRun { dials, warmups, pongs, sent }
}

/// **The trade tape's reconnect waits, its idle and its keepalive, pinned verbatim before its session moved onto the
/// market pump's own dial and stream** (connection-sync, 2026-11-22; the values are the family's pump row's,
/// `vike_bridge_core::websocket_session::spec`'s `row`). For both venues: a session quiet but for one server ping,
/// inside the row's idle limit and its keepalive cadence (the idle-limit audit of 2026-11-22 gave binance's row 540 s
/// and aster's 180 s and a 60 s keepalive, which the link harness's S5 and S11 prove on this tape:
/// `crates/vike-bridge-core/tests/link_conformance/market.rs`), is not cut, the ping is answered and the lane sends
/// nothing of its own; after its close and after each of two refused dials the next dial comes a fixed 3 s later; the
/// REST warm-up runs once, at the first session.
#[test]
fn the_trade_tapes_waits_idle_and_keepalive_are_pinned_verbatim() {
    let quiet = Duration::from_secs(25);
    let text = |frame: String| UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Text(frame)));
    let ping = || UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Ping(Vec::new())));
    // Each venue's `FamilySpec::market_keepalive` (aster's is `vike_aster::feed::KEEPALIVE`, out of this crate's reach).
    for (venue, keepalive) in
        [("binance", None), ("aster", Some(r#"{"method":"LIST_SUBSCRIPTIONS","id":1}"#))]
    {
        let run = tape_run(
            venue,
            keepalive,
            vec![
                Some(vec![
                    text(agg_trade(1)),
                    UserScriptStep::Stall(quiet),
                    ping(),
                    UserScriptStep::Stall(quiet),
                    UserScriptStep::Close { code: 1006, reason: "gone".into() },
                ]),
                None,
                None,
                Some(vec![text(agg_trade(2))]),
            ],
        );
        assert_eq!(run.dials, [0, 53_000, 56_000, 59_000], "{venue}: 3 s after every fault");
        assert_eq!(run.warmups, 1, "{venue}: one REST warm-up, at the first session");
        assert_eq!(run.pongs, [25_000], "{venue}: the server's ping answered");
        assert_eq!(run.sent, Vec::<String>::new(), "{venue}: the lane sends nothing of its own");
    }
}

/// The drain's wall-clock ceiling and the socket's read timeout are ONE span stated twice (see
/// [`DRAIN_DEADLINE`]) — pinned equal here because the drain's contract ("never blocks past one
/// read tick") is a claim about the socket, and the socket's timeout now comes from the row.
#[test]
fn the_drain_deadline_matches_the_rows_read_timeout() {
    assert_eq!(DRAIN_DEADLINE, spec::row("binance").read_timeout);
    assert_eq!(DRAIN_DEADLINE, spec::row("aster").read_timeout);
}

/// A session fault is `Error` with its words unchanged: a fault the connect closure pre-formatted
/// verbatim, a raw one inside the ws-error wrapper. The same fault disclosed after the lane's own stop
/// (the dial-then-stop path's `stopped before warmup`) is `Unknown`: one lane stopping says nothing
/// about the venue's other lanes on the shared line.
#[test]
fn a_session_fault_is_an_error_and_the_lanes_own_stop_claims_nothing() {
    let key = "BTCUSDT@aggTrade";
    let connect = "BTCUSDT@aggTrade connect error (reconnecting): bad ws url";
    assert_eq!(
        session_status_line("Binance", key, false, WebsocketSessionStatus::Error(connect)),
        (DataClientConnectionState::Error, connect.to_string())
    );
    assert_eq!(
        session_status_line("Binance", key, false, WebsocketSessionStatus::Error("reset")),
        (
            DataClientConnectionState::Error,
            "BTCUSDT@aggTrade ws error (reconnecting): reset".to_string()
        )
    );
    let stopped = "BTCUSDT@aggTrade stopped before warmup";
    assert_eq!(
        session_status_line("Binance", key, true, WebsocketSessionStatus::Error(stopped)),
        (DataClientConnectionState::Unknown, stopped.to_string())
    );
}

/// A session the venue announced the end of is a planned reopen: `Connecting` in the notice's words, never `Error`.
#[test]
fn an_announced_end_is_connecting() {
    let words = "the venue sent serverShutdown";
    assert_eq!(
        session_status_line(
            "Binance",
            "BTCUSDT@aggTrade",
            false,
            WebsocketSessionStatus::Announced(words)
        ),
        (
            DataClientConnectionState::Connecting,
            "BTCUSDT@aggTrade reconnecting: the venue sent serverShutdown".to_string()
        )
    );
}

/// **The startup REST warmup is bounded, and bounded to fit the dial.**
///
/// Bounding the dial is not the same as bounding the connect closure, and this is where that
/// gap lived: [`run_trades_feed`]'s closure calls [`fetch_agg_trades_latest`] between the dial
/// and the drain, and it did so on the shared `vike_transport::http::blocking_agent` — a
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

    let shared = vike_transport::http::blocking_agent()
        .config()
        .timeouts()
        .global
        .expect("the shared agent has always carried a global timeout");
    assert!(
        WARMUP_TIMEOUT < shared,
        "the warmup bound ({WARMUP_TIMEOUT:?}) must be shorter than the shared pager agent's \
             ({shared:?}), or this test is satisfied by the defect"
    );

    let dial = spec::row("binance")
        .connect_timeout
        .expect("binance's row bounds its dial — see every_on_driver_row_bounds_its_dial");
    assert!(
        WARMUP_TIMEOUT <= dial,
        "the warmup ({WARMUP_TIMEOUT:?}) is now the LARGEST window a feed thread can be caught \
             in, bigger than the dial ({dial:?}) the recorder's FEED_STOP_BUDGET_SECS is derived \
             from. Shorten it, or re-derive that budget deliberately."
    );
}
