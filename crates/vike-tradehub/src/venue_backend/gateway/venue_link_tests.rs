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

/// The asking end of the venues the gatewayhub's lane reports a path for (`bridge_trade_tests.rs`' twin).
#[test]
fn the_venues_that_report_a_path_are_these() {
    let reporting = [
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

/// `trade health` says UNKNOWN for the venues the wire's list does not name, and it cannot read this table (it links
/// no node): the two are held equal here, and `bridge_trade_tests.rs` holds this one equal to the gatewayhub's lane.
#[test]
fn the_wires_list_of_venues_reporting_a_path_is_this_tables() {
    for venue in vike_model::VENUES {
        assert_eq!(
            reports_path(venue),
            vike_tradehub_client::wire::VENUES_REPORTING_A_PATH.contains(venue),
            "{venue}"
        );
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

#[test]
fn a_down_path_is_heard_with_its_words_once_as_new_and_then_as_a_continuing_outage() {
    let mut watch = PathWatch::armed(Instant::now());
    let report = status(true, Some(down("JForex session disconnected", 0, 5_000)));
    let Reading::Down { down: first, newly } = watch.hear(KEY, &report) else {
        panic!("a down path is a verdict")
    };
    assert!(newly, "the first report of an outage is the loss");
    assert_eq!(
        first,
        VenueLinkDownWire {
            route_key: KEY.to_string(),
            why: "JForex session disconnected".to_string(),
            failed_restarts: 0,
            retry_in_ms: 5_000,
            effect: String::new(),
            no_adapter: false,
        }
    );
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
    let _ = watch.hear(KEY, &status(true, Some(down("gone", 0, 5_000))));
    assert_eq!(
        watch.hear(KEY, &status(true, Some(VenueLinkWire::working()))),
        Reading::Up { was_down: true },
        "the return is worth a line"
    );
}

#[test]
fn a_daemon_with_no_adapter_gives_no_verdict_and_one_with_no_report_ends_an_outage() {
    let mut watch = PathWatch::armed(Instant::now());
    let _ = watch.hear(KEY, &status(true, Some(down("gone", 0, 5_000))));
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
