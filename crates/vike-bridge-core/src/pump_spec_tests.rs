use super::MarketPumpSpec::{NoPump, OnDriver, OwnPump};
use super::*;
use vike_venues::venue_rate_limits::Market;
use vike_venues::venue_timings::timings_for;

/// The CURRENT knob matrix of every on-driver venue, verbatim — any drift in a row is loud
/// here. (The venue side cannot drift by construction: each venue's `pump_opts` consumes its
/// row via [`PumpKnobs::opts`] — row ownership, the `venue_tif` rule.)
#[test]
fn on_driver_knob_matrix_is_pinned() {
    #[rustfmt::skip]
        let rows: &[(&str, PumpKnobs)] = &[
            // binance family (kline + aggTrade lanes; aster shares the family pumps): URL-path
            // subscription, no ack/keepalive/idle, fixed 3 s backoff, 10 s bounded dial.
            ("binance", PumpKnobs { subscribe_frame: false, keepalive_every: None, ack_timeout: None, idle_threshold: None, read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            ("aster",   PumpKnobs { subscribe_frame: false, keepalive_every: None, ack_timeout: None, idle_threshold: None, read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            // bybit/okx: subscribe frame + the br7 10 s ack watchdog.
            ("bybit",   PumpKnobs { subscribe_frame: true, keepalive_every: None, ack_timeout: Some(Duration::from_secs(10)), idle_threshold: None, read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            ("okx",     PumpKnobs { subscribe_frame: true, keepalive_every: None, ack_timeout: Some(Duration::from_secs(10)), idle_threshold: None, read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            // hyperliquid: 30 s app ping + 60 s idle watchdog, no ack.
            ("hyperliquid", PumpKnobs { subscribe_frame: true, keepalive_every: Some(Duration::from_secs(30)), ack_timeout: None, idle_threshold: Some(Duration::from_secs(60)), read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
            // polymarket: 10 s "PING" + 30 s idle + exponential 500 ms → 30 s + 10 s bounded dial.
            ("polymarket", PumpKnobs { subscribe_frame: true, keepalive_every: Some(Duration::from_secs(10)), ack_timeout: None, idle_threshold: Some(Duration::from_secs(30)), read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Exponential { initial: Duration::from_millis(500), max: Duration::from_secs(30) }, connect_timeout: Some(Duration::from_secs(10)) }),
            // deribit: the ONLY row with BOTH an ack watchdog and an app keepalive — a venue that
            // ACKs its subscribe AND answers a `public/test` ping (20 s) behind a 60 s stall watch.
            ("deribit", PumpKnobs { subscribe_frame: true, keepalive_every: Some(Duration::from_secs(20)), ack_timeout: Some(Duration::from_secs(10)), idle_threshold: Some(Duration::from_secs(60)), read_timeout: Duration::from_secs(2), backoff: PumpBackoff::Fixed(Duration::from_secs(3)), connect_timeout: Some(Duration::from_secs(10)) }),
        ];
    for (venue, want) in rows {
        assert_eq!(market_pump_spec(venue), OnDriver(*want), "{venue}");
    }
}

/// **Every on-driver row BOUNDS its dial** — the property, stated once, over whatever rows
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
    for &v in vike_model::VENUES {
        let OnDriver(k) = market_pump_spec(v) else { continue };
        let bound = k.connect_timeout.unwrap_or_else(|| {
                panic!(
                    "{v}: this row rides the shared market pump with connect_timeout: None, which \
                     selects plain `tungstenite::connect` — NO connect bound. A feed thread dialing \
                     a black-holed route then ignores the stop flag for the OS's own SYN ladder \
                     (~127 s on Linux defaults), blowing the recorder's whole teardown budget. Set \
                     `connect_timeout: Some(CONNECT_10S)`, and make sure the venue's dial CONSUMES \
                     it (a venue owning its own connect closure must call \
                     `market_pump::connect_market_socket`, not `tungstenite::connect`)."
                )
            });
        assert!(
            bound <= CONNECT_10S,
            "{v}: a dial bound of {bound:?} exceeds the roster maximum this table publishes \
                 ({CONNECT_10S:?}), which is what the recorder's FEED_STOP_BUDGET_SECS is derived \
                 from. Raise both together, deliberately, or keep the shared window."
        );
        checked += 1;
    }
    assert!(checked >= 6, "the loop must actually see the on-driver rows, saw {checked}");
}

/// Completeness vs the canonical roster (`vike_model::VENUES`): every roster venue is
/// classified exactly once — OnDriver, OwnPump, or a NAMED NoPump (never the unknown-venue
/// fallthrough). Adding a roster venue fails here until its row exists.
#[test]
fn every_roster_venue_is_classified() {
    const ON_DRIVER: &[&str] =
        &["binance", "aster", "bybit", "okx", "hyperliquid", "polymarket", "deribit", "ig"];
    const OWN_PUMP: &[&str] = &["alpaca", "ctrader", "ibkr", "oanda"];
    #[rustfmt::skip]
        const NO_PUMP: &[&str] = &[
            "fxcm", "dukascopy",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): move to ON_DRIVER/OWN_PUMP when a feed lands
        ];
    assert_eq!(
        ON_DRIVER.len() + OWN_PUMP.len() + NO_PUMP.len(),
        vike_model::VENUES.len(),
        "every roster venue classified exactly once"
    );
    for &v in vike_model::VENUES {
        let in_sets = [ON_DRIVER, OWN_PUMP, NO_PUMP].iter().filter(|set| set.contains(&v)).count();
        assert_eq!(in_sets, 1, "roster venue {v} must appear in exactly one class list");
        match market_pump_spec(v) {
            OnDriver(_) => assert!(ON_DRIVER.contains(&v), "{v} row says OnDriver"),
            OwnPump { site } => {
                assert!(OWN_PUMP.contains(&v), "{v} row says OwnPump");
                assert!(!site.is_empty(), "{v} OwnPump row must cite its code");
            }
            NoPump { why } => {
                assert!(NO_PUMP.contains(&v), "{v} row says NoPump");
                assert_ne!(
                    why, UNKNOWN_VENUE_WHY,
                    "roster venue {v} must have a NAMED row, not the unknown fallthrough"
                );
            }
        }
    }
}

/// CROSS-PIN with the `vike_venues::venue_caps` registry — the sibling of
/// `crates/vike-venues/src/venue_tif_tests.rs`'s `venue_caps_cross_pin_the_tif_table`.
///
/// These are TWO independently-maintained declarations of ONE fact — *does this venue have a
/// live market-data feed at all*: this table's class (`OnDriver`/`OwnPump` = yes, `NoPump` =
/// no) and `VenueCaps.live_data` (`has_live_data()`). Each was written by reading the same
/// adapter files; neither is derived from the other; so they must agree for every roster venue.
/// A disagreement matters: `vike_data::require_live_verb` derives `LiveDataError::Unsupported`
/// from the caps row, and `vike_app_core::ui::feed_lifecycle` treats that error as PROVABLY
/// PERMANENT and never retries the feed.
///
/// No exception table, deliberately: a genuine future divergence should be argued in a review,
/// not pre-authorized by an empty allowlist sitting here inviting a row.
///
/// ⚠ Note what this does NOT check: WHICH verbs. This table has no per-verb axis (a venue's
/// depth ladder rides a different driver entirely — see the module doc), so it can only pin
/// the any/none question. Per-verb truth for the six venues that route refusals through
/// `require_live_verb` is pinned by `vike_data::live`'s own
/// `require_live_verb_is_driven_by_the_declared_matrix`.
#[test]
fn venue_caps_cross_pin_the_pump_spec() {
    for &v in vike_model::VENUES {
        let has_pump = !matches!(market_pump_spec(v), NoPump { .. });
        assert_eq!(
            vike_venues::caps_for(v).has_live_data(),
            has_pump,
            "{v}: VenueCaps.live_data and the market-pump table disagree about whether this \
                 venue has a live market feed. One of them is stale — read the adapter, not the \
                 other table. (market_pump_spec says {:?})",
            market_pump_spec(v)
        );
    }
    // The equivalence must be non-vacuous in BOTH directions: a bug that made `has_live_data`
    // or `market_pump_spec` constant would satisfy the loop above for a roster of one class.
    assert!(vike_model::VENUES.iter().any(|v| vike_venues::caps_for(v).has_live_data()));
    assert!(vike_model::VENUES.iter().any(|v| !vike_venues::caps_for(v).has_live_data()));
}

/// Unknown venue strings fall through to the sentinel `NoPump` row.
#[test]
fn unknown_venues_fall_through() {
    assert_eq!(market_pump_spec("no-such-venue"), NoPump { why: UNKNOWN_VENUE_WHY });
}

/// [`PumpKnobs::opts`] assembles the driver opts 1:1 from the row + the venue payloads.
#[test]
fn knobs_assemble_opts() {
    // A frame-subscribing venue with a keepalive (the polymarket shape).
    let k = market_pump_spec("polymarket").knobs();
    let opts = k.opts(Some(r#"{"assets_ids":["1"]}"#), Some("PING"));
    assert_eq!(opts.subscribe, Some(r#"{"assets_ids":["1"]}"#));
    let ka = opts.keepalive.expect("keepalive assembled");
    assert_eq!((ka.payload, ka.every), ("PING", Duration::from_secs(10)));
    assert_eq!(opts.ack_timeout, None);
    assert_eq!(opts.idle_threshold, Some(Duration::from_secs(30)));
    assert_eq!(opts.read_timeout, Duration::from_secs(2));
    assert_eq!(
        opts.backoff,
        PumpBackoff::Exponential {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(30)
        }
    );
    assert_eq!(opts.connect_timeout, Some(Duration::from_secs(10)));

    // A URL-subscribed venue without payloads (the binance-family shape).
    let opts = market_pump_spec("binance").knobs().opts(None, None);
    assert_eq!(opts.subscribe, None);
    assert!(opts.keepalive.is_none());
    assert_eq!(opts.backoff, PumpBackoff::Fixed(Duration::from_secs(3)));
    // The dial bound reaches the driver opts for a URL-subscribed venue too — `opts()` carries
    // the row's `connect_timeout` through unconditionally, so the one venue with no subscribe
    // frame is not quietly the one venue with no bound.
    assert_eq!(opts.connect_timeout, Some(Duration::from_secs(10)));
}

/// `knobs()` on a non-driver row is a static misclassification — it must panic loudly, not
/// hand back fabricated knobs.
#[test]
#[should_panic(expected = "own market pump")]
fn knobs_of_an_own_pump_row_panics() {
    let _ = market_pump_spec("alpaca").knobs();
}

#[test]
#[should_panic(expected = "no market pump")]
fn knobs_of_a_no_pump_row_panics() {
    let _ = market_pump_spec("dukascopy").knobs();
}

/// `subscribe_only_pump_opts` is the venue's own row with the payload filled in and NO keepalive,
/// and it reads the row of the venue it is NAMED, so bybit and okx cannot answer from each other's.
#[test]
fn subscribe_only_pump_opts_is_the_named_venues_row_with_the_payload_and_no_keepalive() {
    for venue in ["bybit", "okx"] {
        let knobs = market_pump_spec(venue).knobs();
        let opts = subscribe_only_pump_opts(venue, "SUB");
        assert_eq!(opts.subscribe, Some("SUB"), "{venue}");
        assert!(opts.keepalive.is_none(), "{venue}: no app-level keepalive on these streams");
        assert_eq!(opts.ack_timeout, knobs.ack_timeout, "{venue}");
        assert_eq!(opts.idle_threshold, knobs.idle_threshold, "{venue}");
        assert_eq!(opts.read_timeout, knobs.read_timeout, "{venue}");
        assert_eq!(opts.backoff, knobs.backoff, "{venue}");
        assert_eq!(opts.connect_timeout, knobs.connect_timeout, "{venue}");
    }
}

// ---------------------------------------------------------------------------------------------
// `vike_venues::venue_timings` (decision 0160): the cross-pins between that table and this
// one, and the mechanism defaults this crate now owns, pinned to the literals they replace.
//
// The two tables declare DIFFERENT things on purpose: this one holds the eight on-driver venues'
// market lanes until the wave that moves them, the other holds everything else. These tests are the
// seam: where both state one fact they agree, no dial outlasts the window the recorder's stop budget
// is derived from, and the table never restates a pump knob it does not own.
// ---------------------------------------------------------------------------------------------

/// The mechanism defaults are the literals the bridges copied, and the on-driver rows all poll at
/// the one public tick. A change to any of these is a deliberate edit to every bridge that still
/// spells the literal (the swap of those bridges onto these constants is a later phase).
#[test]
fn the_mechanism_defaults_are_the_literals_they_replace() {
    // The seven private `READ_TIMEOUT` copies: binance (depth + market_feed), bybit (market_data +
    // market_feed), okx (market_data + market_feed/depth), alpaca (data/conn).
    assert_eq!(READ_POLL, Duration::from_secs(2));
    // The `POLL` of binance, bybit, okx, deribit, polymarket, hyperliquid and the listenKey pump.
    assert_eq!(crate::user_data::USER_DATA_POLL, Duration::from_secs(1));
    // The `backoff` the pump starts from, and the 30 s ceiling every spawn site passes.
    assert_eq!(crate::user_data::USER_DATA_INITIAL_BACKOFF, Duration::from_secs(1));
    assert_eq!(crate::user_data::USER_DATA_MAX_BACKOFF, Duration::from_secs(30));
    // The resync `settle` of binance, bybit, okx, deribit, polymarket and the listenKey pump.
    assert_eq!(crate::user_data::RESYNC_SETTLE, Duration::from_secs(1));
    // The `FILL_SENTINEL` of binance, aster, bybit, okx and deribit.
    assert_eq!(crate::exec_actor::DEFAULT_FILL_SENTINEL, Duration::from_secs(5));

    let mut rows = 0;
    for &v in vike_model::VENUES {
        if let OnDriver(knobs) = market_pump_spec(v) {
            assert_eq!(knobs.read_timeout, READ_POLL, "{v}: an on-driver row polls at READ_POLL");
            rows += 1;
        }
    }
    assert_eq!(rows, 8, "the eight on-driver venues");
    // The recorder's stop budget is this sum (`FEED_STOP_BUDGET_SECS`): raising either term is a
    // deliberate edit there, not a drive-by here.
    assert_eq!(READ_POLL + CONNECT_10S, Duration::from_secs(12));
}

/// Every pump-driven user-data lane declares, in the table, the reconnect policy the pump itself
/// defaults to. The table states it per venue so a reader sees it beside the exceptions (ig, ctrader,
/// oanda, alpaca, ibkr); this is what keeps the two statements one fact.
#[test]
fn every_pump_driven_user_data_lane_declares_the_pumps_backoff() {
    use crate::user_data::{USER_DATA_INITIAL_BACKOFF, USER_DATA_MAX_BACKOFF};
    let pump_driven = ["binance", "aster", "bybit", "okx", "deribit", "polymarket", "hyperliquid"];
    let mut lanes = 0;
    for venue in pump_driven {
        for market in [Market::Spot, Market::Perp] {
            let reconnect = timings_for(venue, market).exec().reconnect();
            assert_eq!(
                (reconnect.initial, reconnect.max),
                (USER_DATA_INITIAL_BACKOFF, USER_DATA_MAX_BACKOFF),
                "{venue} {market:?}: the exec lane's reconnect is the user-data pump's default"
            );
            lanes += 1;
        }
    }
    assert_eq!(lanes, 14, "seven venues, two markets each");
}

/// Where both tables state one fact they agree. Deribit arms on its private side the SAME 20 s ping
/// and 60 s stall watchdog its pump row arms on the public side (the pump row's own comment says
/// so), and its depth lane consumes the pump row's watchdog; hyperliquid shares one ping constant
/// between its user-data pump and its market pump.
#[test]
fn the_tables_agree_where_they_state_one_fact() {
    let OnDriver(pump) = market_pump_spec("deribit") else { panic!("deribit rides the driver") };
    let row = timings_for("deribit", Market::Spot);
    assert_eq!(Some(row.exec().keepalive_every()), pump.keepalive_every, "deribit ping");
    assert_eq!(Some(row.exec().idle_threshold()), pump.idle_threshold, "deribit exec watchdog");
    assert_eq!(Some(row.depth().idle_threshold()), pump.idle_threshold, "deribit depth watchdog");

    let OnDriver(pump) = market_pump_spec("hyperliquid") else {
        panic!("hyperliquid rides the driver")
    };
    let row = timings_for("hyperliquid", Market::Spot);
    assert_eq!(Some(row.exec().keepalive_every()), pump.keepalive_every, "hyperliquid ping");
}

/// No dial the table declares outlasts the window the recorder's stop budget is derived from: a
/// longer bound here is a direct debit against a `TimeoutStopSec=` on a live box, exactly as it is
/// for the pump rows (`every_on_driver_row_bounds_its_dial`).
#[test]
fn no_declared_dial_outlasts_the_connect_window() {
    let mut dials = 0;
    for &v in vike_model::VENUES {
        for market in [Market::Spot, Market::Perp] {
            let row = timings_for(v, market);
            for lane in [row.exec, row.market].into_iter().flatten() {
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

/// The table never restates a knob this crate's rows own: an on-driver venue's `market` lane here
/// may declare only what a `PumpKnobs` row cannot carry (ig's TLCP handshake budget), never a dial,
/// a keepalive, an idle threshold or a reconnect. Two homes for one number is how they drift.
#[test]
fn the_table_never_restates_a_pump_row() {
    let mut on_driver = 0;
    for &v in vike_model::VENUES {
        if !matches!(market_pump_spec(v), OnDriver(_)) {
            continue;
        }
        on_driver += 1;
        let Some(lane) = timings_for(v, Market::Spot).market else { continue };
        assert!(
            lane.dial_timeout.is_none()
                && lane.keepalive_every.is_none()
                && lane.idle_threshold.is_none()
                && lane.reconnect.is_none(),
            "{v}: the market lane restates a `pump_spec` knob; the pump row owns it until it moves"
        );
    }
    assert_eq!(on_driver, 8, "the eight on-driver venues");
}
