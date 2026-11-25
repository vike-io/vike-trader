//! [`PathWatch`], pure: what each kind of report changes, and that an absent adapter or an absent report is never taken
//! for an answer about the path.

use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{TradeStatusReport, VenueLinkWire};

use super::*;

const KEY: &str = "dukascopy";

fn status(adapter_up: bool, link: Option<VenueLinkWire>) -> TradeStatusReport {
    TradeStatusReport { adapter_up, venue_link: link, ..TradeStatusReport::default() }
}

fn down(why: &str, failed: u32, retry_ms: u64) -> VenueLinkWire {
    VenueLinkWire {
        up: false,
        why: why.to_string(),
        failed_restarts: failed,
        retry_in_ms: retry_ms,
        effect: String::new(),
    }
}

/// The asking end of the venues the gatewayhub's lane reports a path for (`bridge_trade_tests.rs`' twin), read off the
/// connection rows: the verdict the two lists it replaced gave.
#[test]
fn the_venues_that_report_a_path_are_these() {
    let reporting = [
        "alpaca",
        "aster",
        "binance",
        "bybit",
        "ctrader",
        "deribit",
        "dukascopy",
        "fxcm",
        "hyperliquid",
        "ibkr",
        "ig",
        "oanda",
        "okx",
        "polymarket",
    ];
    for venue in vike_model::VENUES {
        assert_eq!(reports_path(venue), reporting.contains(venue), "{venue}");
    }
}

/// **A return is said in the words of what was lost**, for every roster venue, read off the connection rows: a stream
/// (the shared pump's, OANDA's included), an order session (fxcm, ibkr, ig), and every other path, whose loss refused the
/// orders.
#[test]
fn each_venues_return_is_said_in_the_words_its_path_earns() {
    let stream = "the bridge reopened it and what the gap holds is read back";
    let session = "the bridge says its session with the venue answers again";
    let refusing = "the bridge has it back and orders are accepted again";
    let words = [
        ("alpaca", stream),
        ("aster", stream),
        ("binance", stream),
        ("bybit", stream),
        ("deribit", stream),
        ("hyperliquid", stream),
        ("oanda", stream),
        ("okx", stream),
        ("polymarket", stream),
        ("fxcm", session),
        ("ibkr", session),
        ("ig", session),
    ];
    // ctrader, dukascopy, and a scaffolded venue, whose row reports nothing yet.
    for venue in vike_model::VENUES {
        let want = words.iter().find(|(v, _)| v == venue).map_or(refusing, |(_, w)| *w);
        assert_eq!(back_words(venue), want, "{venue}");
    }
}

/// **The words about orders are the bridge's**: a stream whose loss refuses no order must not read as one that does,
/// neither going down nor coming back; a sidecar's return keeps its own sentence.
#[test]
fn an_outage_carries_the_bridges_own_words_about_orders() {
    let mut watch = PathWatch::armed(Instant::now());
    let link =
        VenueLinkWire { effect: "orders still go out".to_string(), ..down("401", 3, 30_000) };
    let Reading::Down { down: heard, .. } = watch.hear("oanda", &status(true, Some(link))) else {
        panic!("a down path is a verdict")
    };
    assert_eq!(heard.effect, "orders still go out");
    assert!(!back_words("oanda").contains("orders are accepted"), "{}", back_words("oanda"));
    assert!(!back_words("binance").contains("orders are accepted"), "{}", back_words("binance"));
    for refusing in ["dukascopy", "ctrader"] {
        assert!(back_words(refusing).contains("orders are accepted again"), "{refusing}");
    }
}

#[test]
fn the_first_ask_is_due_at_once_and_the_next_one_period_later() {
    let t0 = Instant::now();
    let mut watch = PathWatch::armed(t0);
    assert!(watch.due(t0));
    watch.asked(t0);
    assert!(!watch.due(t0 + ASK_EVERY - Duration::from_millis(1)));
    assert!(watch.due(t0 + ASK_EVERY));
}

/// A sidecar that died is a failure before any restart failed (its bridge counts the death as one,
/// `vike_bridge_core::venue_link::VenueLinkDown::failed`): said at the first ask that finds it (its `0` used to read as a
/// routine reopen, said one ask late), and then a continuing outage.
#[test]
fn a_down_path_is_heard_with_its_words_once_as_new_and_then_as_a_continuing_outage() {
    let mut watch = PathWatch::armed(Instant::now());
    let report = status(true, Some(down("JForex session disconnected", 1, 5_000)));
    let Reading::Down { down: first, newly } = watch.hear(KEY, &report) else {
        panic!("a down path is a verdict")
    };
    assert!(newly, "a dead sidecar is said at the first ask that finds it");
    assert_eq!(
        first,
        VenueLinkDownWire {
            route_key: KEY.to_string(),
            why: "JForex session disconnected".to_string(),
            failed_restarts: 1,
            retry_in_ms: 5_000,
            effect: String::new(),
            no_adapter: false,
        }
    );
    let Reading::Down { newly, .. } = watch.hear(KEY, &report) else { panic!("still a verdict") };
    assert!(!newly, "the same outage is not said twice");
    let worse = status(true, Some(down("restart refused", 3, 40_000)));
    let Reading::Down { down: later, newly } = watch.hear(KEY, &worse) else {
        panic!("still down")
    };
    assert!(!newly, "the same outage is not a second loss");
    assert_eq!(
        (later.why.as_str(), later.failed_restarts, later.retry_in_ms),
        ("restart refused", 3, 40_000)
    );
}

#[test]
fn an_up_path_clears_the_outage_and_says_it_was_one() {
    let mut watch = PathWatch::armed(Instant::now());
    assert_eq!(
        watch.hear(KEY, &status(true, Some(VenueLinkWire::working()))),
        Reading::Up { was_down: false }
    );
    let _ = watch.hear(KEY, &status(true, Some(down("gone", 1, 5_000))));
    assert_eq!(
        watch.hear(KEY, &status(true, Some(VenueLinkWire::working()))),
        Reading::Up { was_down: true },
        "the return is worth a line"
    );
}

/// **A routine reconnect says nothing** (the CI box: hyperliquid's testnet ends its private stream every ~10 min with
/// `1000 Expired` and the pump is back in about 1.5 s, so an ask every 5 s finds it down about a third of the time, and
/// each one used to be a DOWN line and a back line). A reopen that one ask finds is a verdict and no line; one that
/// outlives an ask is said once, and so is its return.
#[test]
fn a_reopen_within_one_ask_says_nothing_and_one_that_outlives_it_is_said_once() {
    let mut watch = PathWatch::armed(Instant::now());
    let reopening = status(true, Some(down("server closed: 1000 Expired", 0, 1_000)));
    let working = status(true, Some(VenueLinkWire::working()));
    assert!(matches!(watch.hear(KEY, &reopening), Reading::Down { newly: false, .. }));
    assert_eq!(watch.hear(KEY, &working), Reading::Up { was_down: false }, "no return to say");

    assert!(matches!(watch.hear(KEY, &reopening), Reading::Down { newly: false, .. }));
    assert!(matches!(watch.hear(KEY, &reopening), Reading::Down { newly: true, .. }));
    assert!(matches!(watch.hear(KEY, &reopening), Reading::Down { newly: false, .. }));
    assert_eq!(watch.hear(KEY, &working), Reading::Up { was_down: true });
}

/// **A failure is said at the first ask that finds it**: a reopen that failed (`Error`) needs no second ask, and a reopen
/// that turns into a failure is said when it does, once.
#[test]
fn a_failure_is_said_at_once_and_a_reopen_that_fails_is_said_when_it_fails() {
    let mut watch = PathWatch::armed(Instant::now());
    let failed = status(true, Some(down("connection refused", 1, 2_000)));
    assert!(matches!(watch.hear(KEY, &failed), Reading::Down { newly: true, .. }));
    assert_eq!(watch.hear(KEY, &status(true, None)), Reading::Up { was_down: true });

    let mut watch = PathWatch::armed(Instant::now());
    let reopening = status(true, Some(down("server closed", 0, 1_000)));
    assert!(matches!(watch.hear(KEY, &reopening), Reading::Down { newly: false, .. }));
    assert!(matches!(watch.hear(KEY, &failed), Reading::Down { newly: true, .. }));
    assert!(matches!(watch.hear(KEY, &failed), Reading::Down { newly: false, .. }));
}

#[test]
fn a_daemon_with_no_adapter_gives_no_verdict_and_one_with_no_report_ends_an_outage() {
    let mut watch = PathWatch::armed(Instant::now());
    let _ = watch.hear(KEY, &status(true, Some(down("gone", 1, 5_000))));
    // A restarted daemon has no adapter yet: its report has no `venue_link` because there is no adapter, which says
    // nothing about the path.
    assert_eq!(watch.hear(KEY, &status(false, None)), Reading::Nothing);
    // An adapter that is up and reports no path: a daemon that predates the field, or a lane that names none.
    assert_eq!(watch.hear(KEY, &status(true, None)), Reading::Up { was_down: true });
}

/// **A live adapter that reports no path is no reason to stop looking**: alpaca reports none, and an order session's
/// bridge reports none while it cannot vouch either way, and their bridges can panic like any other. The watch used to
/// end at that first answer, so the panic read `HEALTH: OK`.
#[test]
fn a_live_adapter_that_reports_no_path_still_gets_the_verdict_when_it_dies() {
    let mut watch = PathWatch::armed(Instant::now());
    assert_eq!(watch.hear(KEY, &status(true, None)), Reading::Up { was_down: false });
    for _ in 1..NO_ADAPTER_ASKS {
        assert_eq!(watch.hear(KEY, &status(false, None)), Reading::Nothing);
    }
    let Reading::Down { down: heard, newly } = watch.hear(KEY, &status(false, None)) else {
        panic!("the last ask is the verdict")
    };
    assert!(newly && heard.no_adapter, "{heard:?}");
    assert_eq!(watch.hear(KEY, &status(true, None)), Reading::Up { was_down: true });
}

/// **A refused restart said why, and the watch's plainer words do not bury it**; they do replace a path outage, because
/// no adapter outranks whatever its path last said.
#[test]
fn the_words_of_a_refusal_outlive_the_watchs_and_no_adapter_outranks_a_path_outage() {
    use crate::server::symbol_admission::LinkHealth as _;
    let facts = super::super::trade_lease_registry::TradeLeaseRegistry::default();
    facts.set_venue_link_down(KEY, Some(no_adapter(KEY, "the venue refused the login")));
    facts.note_no_adapter(KEY, no_adapter(KEY, "the watch's words"));
    assert_eq!(facts.venue_link_down()[0].why, "the venue refused the login");

    let mut watch = PathWatch::armed(Instant::now());
    let Reading::Down { down: outage, .. } =
        watch.hear(KEY, &status(true, Some(down("gone", 1, 5))))
    else {
        panic!("a down path is a verdict")
    };
    facts.set_venue_link_down(KEY, Some(outage));
    facts.note_no_adapter(KEY, no_adapter(KEY, "the watch's words"));
    assert!(facts.venue_link_down()[0].no_adapter);
    assert_eq!(facts.venue_link_down()[0].why, "the watch's words");
}

/// **A bridge that panicked marks its adapter dead, the lease stays up and nothing here restarts it**: `adapter_up` stays
/// false for good, and this watch used to answer that with no verdict for ever, so `trade health` read `PASS engine
/// RUNNING` and `HEALTH: OK` with every order refused. About 30 s of asks is the verdict, once as new, and an adapter that
/// comes back ends it. The count restarts, so the seconds of a gatewayhub restart never add up to it.
#[test]
fn an_adapter_missing_for_about_thirty_seconds_is_a_verdict_and_its_return_ends_it() {
    let mut watch = PathWatch::armed(Instant::now());
    assert_eq!(ASK_EVERY * NO_ADAPTER_ASKS, Duration::from_secs(30));
    for _ in 1..NO_ADAPTER_ASKS {
        assert_eq!(watch.hear(KEY, &status(false, None)), Reading::Nothing);
    }
    let Reading::Down { down: heard, newly } = watch.hear(KEY, &status(false, None)) else {
        panic!("the last ask is the verdict")
    };
    assert!(newly && heard.no_adapter, "{heard:?}");
    assert!(heard.say().contains("NO LIVE ADAPTER"), "{}", heard.say());
    assert!(heard.say().contains("refused"), "{}", heard.say());
    let Reading::Down { newly, .. } = watch.hear(KEY, &status(false, None)) else {
        panic!("it stays the verdict")
    };
    assert!(!newly, "said once");

    assert_eq!(
        watch.hear(KEY, &status(true, Some(VenueLinkWire::working()))),
        Reading::Up { was_down: true }
    );
    for _ in 1..NO_ADAPTER_ASKS {
        assert_eq!(watch.hear(KEY, &status(false, None)), Reading::Nothing, "the count restarted");
    }
}
