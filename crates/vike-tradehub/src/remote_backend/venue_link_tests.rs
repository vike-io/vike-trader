//! [`PathWatch`], pure: which venues are watched, what each kind of report changes, and that an absent adapter or an
//! absent report is never taken for an answer about the path.

use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{TradeStatusReport, WireVenueLink};

use super::*;

const KEY: &str = "dukascopy";

fn status(adapter_up: bool, link: Option<WireVenueLink>) -> TradeStatusReport {
    TradeStatusReport { adapter_up, venue_link: link, ..TradeStatusReport::default() }
}

fn down(why: &str, failed: u32, retry_ms: u64) -> WireVenueLink {
    WireVenueLink {
        up: false,
        why: why.to_string(),
        failed_restarts: failed,
        retry_in_ms: retry_ms,
    }
}

#[test]
fn only_the_venue_whose_exec_is_an_owned_child_is_watched() {
    let now = Instant::now();
    assert!(PathWatch::armed("dukascopy", now).is_some());
    for venue in vike_model::VENUES.iter().filter(|v| **v != "dukascopy") {
        assert!(PathWatch::armed(venue, now).is_none(), "{venue} is asked nothing, as before");
    }
}

#[test]
fn the_first_ask_is_due_at_once_and_the_next_one_period_later() {
    let t0 = Instant::now();
    let mut watch = PathWatch::armed("dukascopy", t0).expect("armed");
    assert!(watch.due(t0));
    watch.asked(t0);
    assert!(!watch.due(t0 + ASK_EVERY - Duration::from_millis(1)));
    assert!(watch.due(t0 + ASK_EVERY));
}

#[test]
fn a_down_path_is_heard_with_its_words_once_as_new_and_then_as_a_continuing_outage() {
    let mut watch = PathWatch::armed("dukascopy", Instant::now()).expect("armed");
    let report = status(true, Some(down("JForex session disconnected", 0, 5_000)));
    let Reading::Down { down: first, newly } = watch.hear(KEY, &report) else {
        panic!("a down path is a verdict")
    };
    assert!(newly, "the first report of an outage is the loss");
    assert_eq!(
        first,
        WireVenueLinkDown {
            route_key: KEY.to_string(),
            why: "JForex session disconnected".to_string(),
            failed_restarts: 0,
            retry_in_ms: 5_000,
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
    let mut watch = PathWatch::armed("dukascopy", Instant::now()).expect("armed");
    assert_eq!(
        watch.hear(KEY, &status(true, Some(WireVenueLink::working()))),
        Reading::Up { was_down: false }
    );
    let _ = watch.hear(KEY, &status(true, Some(down("gone", 0, 5_000))));
    assert_eq!(
        watch.hear(KEY, &status(true, Some(WireVenueLink::working()))),
        Reading::Up { was_down: true },
        "the return is worth a line"
    );
    assert!(!watch.is_over(), "a path that reports keeps being asked");
}

#[test]
fn a_daemon_with_no_adapter_gives_no_verdict_and_one_with_no_report_ends_the_watch() {
    let mut watch = PathWatch::armed("dukascopy", Instant::now()).expect("armed");
    let _ = watch.hear(KEY, &status(true, Some(down("gone", 0, 5_000))));
    // A restarted daemon has no adapter yet: its report has no `venue_link` because there is no adapter, which says
    // nothing about the path (and must not end the watch, or the outage would never be seen again).
    assert_eq!(watch.hear(KEY, &status(false, None)), Reading::Nothing);
    assert!(!watch.is_over());
    // An adapter that is up and reports no path: a daemon that predates the field, or a lane that names none.
    assert_eq!(watch.hear(KEY, &status(true, None)), Reading::Up { was_down: true });
    assert!(watch.is_over(), "nothing more is asked of a lease that reports no path");
}
