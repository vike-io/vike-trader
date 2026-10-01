use super::MarketPumpSpec::{NoPump, OnDriver, OwnPump};
use super::*;

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
/// exist, so a venue added tomorrow inherits it instead of inheriting the hole.
///
/// The matrix above already pins each row verbatim, and it did so while five of the six rows
/// carried `connect_timeout: None`: a verbatim pin proves a table has not CHANGED, never that
/// what it says is safe. This is the invariant test beside it, and it is the one that would have
/// gone red on the defect — the binance/aster/bybit/okx/hyperliquid rows were all unbounded and
/// every test in this file passed.
///
/// The ceiling is asserted too, because the failure mode of over-correcting is silent: this
/// window is spent from `crates/vike-datahub/src/recorder.rs`'s
/// `FEED_STOP_BUDGET_SECS`, which is [`READ_2S`] plus the LARGEST dial on the roster. A row
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

/// CROSS-PIN with the `vike_model::venue_caps` registry — the sibling of
/// `crates/vike-model/src/venue_tif.rs`'s `venue_caps_cross_pin_the_tif_table`, and the check
/// whose absence let a wrong row live for three weeks.
///
/// These are TWO independently-maintained declarations of ONE fact — *does this venue have a
/// live market-data feed at all*: this table's class (`OnDriver`/`OwnPump` = yes, `NoPump` =
/// no) and `VenueCaps.live_data` (`has_live_data()`). Each was written by reading the same
/// adapter files; neither is derived from the other; so they must agree for every roster venue.
///
/// They did NOT agree. `venue_caps::IBKR` declared `LiveDataCaps::NONE` while this table's
/// `"ibkr"` row said `OwnPump` and CITED `vike-ibkr market_feed/mod.rs` — the file implementing
/// all five `DataClient` verbs (#306, merged the day after the caps row was authored). The two
/// tables sat one crate apart contradicting each other and every test passed, because no test
/// had ever compared them. That mattered: `vike_data::require_live_verb` derives
/// `LiveDataError::Unsupported` from the caps row, and `vike_app_core::ui::feed_lifecycle` treats
/// that error as PROVABLY PERMANENT and never retries the feed.
///
/// No exception table, deliberately — there is nothing to except. The equivalence holds for all
/// 14 roster venues today, and a genuine future divergence should be argued in a review, not
/// pre-authorized by an empty allowlist sitting here inviting a row.
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
            vike_model::caps_for(v).has_live_data(),
            has_pump,
            "{v}: VenueCaps.live_data and the market-pump table disagree about whether this \
                 venue has a live market feed. One of them is stale — read the adapter, not the \
                 other table. (market_pump_spec says {:?})",
            market_pump_spec(v)
        );
    }
    // The equivalence must be non-vacuous in BOTH directions: a bug that made `has_live_data`
    // or `market_pump_spec` constant would satisfy the loop above for a roster of one class.
    assert!(vike_model::VENUES.iter().any(|v| vike_model::caps_for(v).has_live_data()));
    assert!(vike_model::VENUES.iter().any(|v| !vike_model::caps_for(v).has_live_data()));
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
