use super::*;
use vike_venues::venue_connection::VenueConnectionDriver::MarketPump;

/// The roster venues whose market lane rides the shared pump, by the one class table.
fn pump_venues() -> Vec<&'static str> {
    vike_model::VENUES
        .iter()
        .copied()
        .filter(|&v| connection_for(v, VenueRateLimitsMarket::Spot).market.driver == MarketPump)
        .collect()
}

/// A row's knobs with its ladder as `(initial, max)`, so a pin states the numbers and not the table's `why`.
type KnobsPin = (
    bool,
    Option<Duration>,
    Option<Duration>,
    Option<Duration>,
    Duration,
    (Duration, Duration),
    Option<Duration>,
);

fn pin(k: WebsocketSessionKnobs) -> KnobsPin {
    (
        k.subscribe_frame,
        k.keepalive_every,
        k.ack_timeout,
        k.idle_threshold,
        k.read_timeout,
        (k.backoff.initial, k.backoff.max),
        k.connect_timeout,
    )
}

fn s(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

/// **The knob matrix of every pump venue, verbatim**: the literals the per-venue arms of this file held before the
/// pump read the venue tables (B3.07), so a row the tables change or a rule [`WebsocketSessionKnobs::from_lanes`]
/// changes is loud here. Columns: subscribe frame, keepalive, ack watchdog, idle watchdog, read poll, reconnect
/// `(initial, max)`, dial. The pinned set is exactly the venues whose market lane rides the pump, and each declares the
/// same driver, subscribe frame and timing lane for both markets, which is what lets [`row`] read the spot row (a
/// row's announced close may differ by market, binance's does: the bridge arms that hook, not the knobs).
#[test]
fn on_driver_knob_matrix_is_pinned() {
    #[rustfmt::skip]
    let rows: &[(&str, KnobsPin)] = &[
        // binance family (kline, mark + trade lanes; aster shares the family pumps): URL-path subscription, no ack or
        // keepalive (the server pings), fixed 3 s, 10 s dial; 540 s is three of its futures books' 180 s pings.
        ("binance",     (false, None,        None,        Some(s(540)), s(2),  (s(3), s(3)), Some(s(10)))),
        // aster: a `LIST_SUBSCRIPTIONS` every 60 s against its perp host's 240 s cut, behind a 180 s watch.
        ("aster",       (false, Some(s(60)), None,        Some(s(180)), s(2),  (s(3), s(3)), Some(s(10)))),
        // bybit/okx: subscribe frame + the 10 s ack watchdog, each venue's own ping behind a 60 s watch.
        ("bybit",       (true,  Some(s(20)), Some(s(10)), Some(s(60)),  s(2),  (s(3), s(3)), Some(s(10)))),
        ("okx",         (true,  Some(s(15)), Some(s(10)), Some(s(60)),  s(2),  (s(3), s(3)), Some(s(10)))),
        // hyperliquid: 30 s app ping + 60 s idle watchdog, no ack.
        ("hyperliquid", (true,  Some(s(30)), None,        Some(s(60)),  s(2),  (s(3), s(3)), Some(s(10)))),
        // polymarket: 10 s "PING" + 30 s idle + exponential 500 ms -> 30 s.
        ("polymarket",  (true,  Some(s(10)), None,        Some(s(30)),  s(2),  (Duration::from_millis(500), s(30)), Some(s(10)))),
        // deribit: ACKs its subscribe and answers a `public/test` ping (20 s) behind a 60 s watch.
        ("deribit",     (true,  Some(s(20)), Some(s(10)), Some(s(60)),  s(2),  (s(3), s(3)), Some(s(10)))),
        // ig: the TLCP handshake rides the connect hook (no frame), SUBOK is the 10 s ack, PROBE keeps the 60 s watch fed.
        ("ig",          (false, None,        Some(s(10)), Some(s(60)),  s(2),  (s(3), s(3)), Some(s(10)))),
        // alpaca: the lanes share a stream's one socket, which subscribes for them; a 180 s watch on the socket.
        ("alpaca",      (false, None,        None,        Some(s(180)), s(2),  (s(3), s(3)), Some(s(10)))),
        // oanda: the chunked-HTTP pricing stream as lines; its response-head wait makes it read at its 20 s idle bound.
        ("oanda",       (false, None,        None,        Some(s(20)),  s(20), (s(3), s(3)), Some(s(10)))),
    ];
    for (venue, want) in rows {
        assert_eq!(pin(row(venue)), *want, "{venue}");
    }
    let pinned: Vec<&str> = rows.iter().map(|(venue, _)| *venue).collect();
    let mut pump = pump_venues();
    pump.sort_unstable();
    let mut sorted = pinned.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, pump, "the pin names exactly the venues whose market lane rides the pump");
    for venue in pinned {
        let [spot, perp] = [VenueRateLimitsMarket::Spot, VenueRateLimitsMarket::Perp];
        let [s, p] = [spot, perp].map(|market| connection_for(venue, market).market);
        assert_eq!((s.driver, s.subscribe_frame), (p.driver, p.subscribe_frame), "{venue}");
        assert_eq!(timings_for(venue, spot).market, timings_for(venue, perp).market, "{venue}");
    }
    // IBKR's Client Portal market lane, read by name: no frame, no keepalive (the REST tickle renews the session), no
    // ack; the 60 s idle limit over the gateway's 10 s heartbeat and the fixed 2 s redial; the lane's 200 ms poll.
    assert_eq!(
        pin(IBKR_CPAPI),
        (false, None, None, Some(s(60)), Duration::from_millis(200), (s(2), s(2)), Some(s(10))),
        "ibkr cpapi"
    );
}

/// **Every pump row BOUNDS its dial** — the property, stated once, over whatever rows
/// exist, so a venue added tomorrow inherits it instead of inheriting the hole. A verbatim pin
/// (the matrix above) proves a table has not CHANGED, never that what it says is safe; this is
/// the invariant beside it.
///
/// The ceiling is asserted too, because the failure mode of over-correcting is silent: this
/// window is spent from `crates/vike-datahub/src/recorder.rs`'s
/// `FEED_STOP_BUDGET_SECS`, which is [`READ_POLL`] plus the LARGEST dial on the roster. A row
/// that quietly doubled its bound would push a live daemon's teardown past its unit's
/// `TimeoutStopSec=` and be paid for in lost tape, not in a failing test — so the roster max is
/// the thing checked, and raising it is a deliberate edit here plus a re-derivation there.
#[test]
fn every_on_driver_row_bounds_its_dial() {
    let mut checked = 0;
    for v in pump_venues() {
        let bound = row(v).connect_timeout.unwrap_or_else(|| {
            panic!(
                "{v}: this row rides the shared market pump with connect_timeout: None, which \
                 selects plain `tungstenite::connect` — NO connect bound. A feed thread dialing \
                 a black-holed route then ignores the stop flag for the OS's own SYN ladder \
                 (~127 s on Linux defaults), blowing the recorder's whole teardown budget."
            )
        });
        assert!(
            bound <= CONNECT_10S,
            "{v}: a dial bound of {bound:?} exceeds the roster maximum this file publishes \
             ({CONNECT_10S:?}), which is what the recorder's FEED_STOP_BUDGET_SECS is derived \
             from. Raise both together, deliberately, or keep the shared window."
        );
        checked += 1;
    }
    assert!(checked >= 6, "the loop must actually see the pump rows, saw {checked}");
    // The row read by name, which the roster loop does not see.
    assert!(IBKR_CPAPI.connect_timeout.is_some_and(|bound| bound <= CONNECT_10S), "ibkr cpapi");
}

/// [`WebsocketSessionKnobs::opts`] assembles the driver opts 1:1 from the row + the venue payloads.
#[test]
fn knobs_assemble_opts() {
    // A frame-subscribing venue with a keepalive (the polymarket shape).
    let opts = row("polymarket").opts(Some(r#"{"assets_ids":["1"]}"#), Some("PING"));
    assert_eq!(opts.subscribe, Some(r#"{"assets_ids":["1"]}"#));
    let ka = opts.keepalive.expect("keepalive assembled");
    assert_eq!((ka.payload, ka.every), ("PING", Duration::from_secs(10)));
    assert_eq!(opts.ack_timeout, None);
    assert_eq!(opts.idle_threshold, Some(Duration::from_secs(30)));
    assert_eq!(opts.read_timeout, Duration::from_secs(2));
    assert_eq!(
        (opts.backoff.initial, opts.backoff.max),
        (Duration::from_millis(500), Duration::from_secs(30))
    );
    assert_eq!(opts.connect_timeout, Some(Duration::from_secs(10)));

    // A URL-subscribed venue without payloads (the binance-family shape).
    let opts = row("binance").opts(None, None);
    assert_eq!(opts.subscribe, None);
    assert!(opts.keepalive.is_none());
    assert_eq!((opts.backoff.initial, opts.backoff.max), (s(3), s(3)));
    // The dial bound reaches the driver opts for a URL-subscribed venue too — `opts()` carries
    // the row's `connect_timeout` through unconditionally, so the one venue with no subscribe
    // frame is not quietly the one venue with no bound.
    assert_eq!(opts.connect_timeout, Some(Duration::from_secs(10)));
}

/// A row for a venue whose market lane is not on the pump is a static misclassification — it must panic loudly, not
/// hand back fabricated knobs: a native session (ctrader), and E16's native price lanes (fxcm, dukascopy).
#[test]
#[should_panic(expected = "not on the shared market pump")]
fn the_row_of_a_native_session_panics() {
    let _ = row("ctrader");
}

#[test]
#[should_panic(expected = "not on the shared market pump")]
fn the_row_of_a_venue_off_the_pump_panics() {
    let _ = row("dukascopy");
}

// ---------------------------------------------------------------------------------------------
// `vike_venues::venue_timings` (decision 0160): the mechanism defaults this crate owns, pinned to the literals they
// replace, and the facts the table states twice on purpose (a venue's one ping rule on its two lanes).
// ---------------------------------------------------------------------------------------------

/// The mechanism defaults are the literals the bridges copied, and the pump rows all poll at
/// the one public tick. A change to any of these is a deliberate edit to every bridge that still
/// spells the literal (the swap of those bridges onto these constants is a later phase).
#[test]
fn the_mechanism_defaults_are_the_literals_they_replace() {
    // The seven private `READ_TIMEOUT` copies: binance (depth + market_feed), bybit (market_data +
    // market_feed), okx (market_data + market_feed/depth), alpaca (data/conn).
    assert_eq!(READ_POLL, Duration::from_secs(2));
    // The `POLL` of binance, bybit, okx, deribit, polymarket, hyperliquid and the listenKey pump.
    assert_eq!(crate::websocket_session::user::USER_DATA_POLL, Duration::from_secs(1));
    // The resync `settle` of binance, bybit, okx, deribit, polymarket and the listenKey pump.
    assert_eq!(crate::websocket_session::user::RESYNC_SETTLE, Duration::from_secs(1));
    // The `FILL_SENTINEL` of binance, aster, bybit, okx and deribit.
    assert_eq!(crate::exec_actor::DEFAULT_FILL_SENTINEL, Duration::from_secs(5));

    let mut rows = 0;
    for v in pump_venues() {
        let lane = timings_for(v, VenueRateLimitsMarket::Spot).market();
        // A lane with a response-head wait (oanda's HTTP body) reads at its idle bound: ureq caps that wait by the
        // per-read bound, and that bound is its one watchdog.
        let poll = if lane.response_timeout.is_some() { lane.idle_threshold() } else { READ_POLL };
        assert_eq!(row(v).read_timeout, poll, "{v}: a pump row polls at READ_POLL");
        rows += 1;
    }
    assert_eq!(rows, 10, "the ten pump venues");
    // The recorder's stop budget is this sum (`FEED_STOP_BUDGET_SECS`): raising either term is a
    // deliberate edit there, not a drive-by here.
    assert_eq!(READ_POLL + CONNECT_10S, Duration::from_secs(12));
}

/// Where the table states one fact on two lanes, the two agree. Deribit arms on its private side the SAME 20 s ping
/// and 60 s stall watchdog its market lane arms on the public side, and its depth lane takes the market lane's
/// watchdog; hyperliquid shares one ping constant between its user-data pump and its market pump.
#[test]
fn the_tables_agree_where_they_state_one_fact() {
    let pump = row("deribit");
    let timings = timings_for("deribit", VenueRateLimitsMarket::Spot);
    assert_eq!(Some(timings.exec().keepalive_every()), pump.keepalive_every, "deribit ping");
    assert_eq!(Some(timings.exec().idle_threshold()), pump.idle_threshold, "deribit exec watchdog");
    assert_eq!(
        Some(timings.depth().idle_threshold()),
        pump.idle_threshold,
        "deribit depth watchdog"
    );

    let pump = row("hyperliquid");
    let timings = timings_for("hyperliquid", VenueRateLimitsMarket::Spot);
    assert_eq!(Some(timings.exec().keepalive_every()), pump.keepalive_every, "hyperliquid ping");

    // bybit and okx ping their public sockets with the cadence their private stream pings at, and watch the same
    // silence (connection-sync C6: the venue's one ping rule, and a pong answering every ping).
    for venue in ["bybit", "okx"] {
        let pump = row(venue);
        let exec = timings_for(venue, VenueRateLimitsMarket::Spot).exec();
        assert_eq!(Some(exec.keepalive_every()), pump.keepalive_every, "{venue} ping");
        assert_eq!(Some(exec.idle_threshold()), pump.idle_threshold, "{venue} watchdog");
    }

    // aster's market lanes talk at its private stream's cadence, against the one 240 s cut of a silent client (the
    // idle-limit audit of 2026-11-22).
    let pump = row("aster");
    let exec = timings_for("aster", VenueRateLimitsMarket::Perp).exec();
    assert_eq!(Some(exec.keepalive_every()), pump.keepalive_every, "aster keepalive");
}

/// No dial the table declares outlasts the window the recorder's stop budget is derived from: a
/// longer bound here is a direct debit against a `TimeoutStopSec=` on a live box, exactly as it is
/// for the pump rows (`every_on_driver_row_bounds_its_dial`).
#[test]
fn no_declared_dial_outlasts_the_connect_window() {
    let mut dials = 0;
    for &v in vike_model::VENUES {
        for market in [VenueRateLimitsMarket::Spot, VenueRateLimitsMarket::Perp] {
            let timings = timings_for(v, market);
            for lane in [timings.exec, timings.market].into_iter().flatten() {
                if let Some(dial) = lane.dial_timeout {
                    assert!(
                        dial.value <= CONNECT_10S,
                        "{v} {market:?}: a dial bound of {:?} exceeds {CONNECT_10S:?}, the window \
                         the recorder's FEED_STOP_BUDGET_SECS is derived from",
                        dial.value
                    );
                    dials += 1;
                }
            }
        }
    }
    assert!(dials >= 3, "oanda's two lanes and alpaca's exec declare a dial; saw {dials}");
}
