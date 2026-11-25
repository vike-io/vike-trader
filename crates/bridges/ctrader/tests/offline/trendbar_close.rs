//! `vike_ctrader::feed::bar::TrendbarCloses`, the sink every frame of a connection's actor passes through:
//! a live trendbar closes when a later period's snapshot arrives, a seed carries no forming bar, an outage or a key
//! subscribed or released again forgets what was held, and a reconnect's fill closes what the outage hid.

use std::sync::Arc;

use vike_ctrader::feed::bar::TrendbarCloses;
use vike_ctrader::test_support::RecordingSink;
use vike_data::DataClientSink;
use vike_marketdata::{Bar, TickQuote};
use vike_model::now_ms;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn closes() -> (Arc<RecordingSink>, TrendbarCloses) {
    let rec = Arc::new(RecordingSink::default());
    (Arc::clone(&rec), TrendbarCloses::new(rec))
}

/// The open of the minute that holds `now`.
fn this_minute() -> i64 {
    now_ms() / MINUTE * MINUTE
}

/// **The last snapshot of a period is closed by the first of the next**, stamped with its own open; another
/// interval or symbol of the stream closes nothing of it, and every other verb passes unchanged.
#[test]
fn a_later_periods_snapshot_closes_the_last_one_held() {
    let (rec, sink) = closes();
    let t0 = 1_000 * MINUTE;
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(t0, 1.1));
    sink.forming_bar("ctrader", "EURUSD", "1h", bar(t0, 1.0));
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(t0, 1.2));
    sink.forming_bar("ctrader", "GBPUSD", "1m", bar(t0 + MINUTE, 1.3));
    sink.quote(
        "ctrader",
        "EURUSD",
        TickQuote {
            timestamp: t0,
            local_timestamp: t0,
            bid: 1.1,
            ask: 1.2,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: "EURUSD".into(),
        },
    );
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(t0 + MINUTE, 1.4));
    assert_eq!(
        rec.calls(),
        [
            "forming_bar(ctrader,EURUSD,1m,1.1)",
            "forming_bar(ctrader,EURUSD,1h,1)",
            "forming_bar(ctrader,EURUSD,1m,1.2)",
            "forming_bar(ctrader,GBPUSD,1m,1.3)",
            "quote:ctrader:EURUSD:1.1/1.2:0x0",
            "close_bar(ctrader,EURUSD,1m,1.2)",
            "forming_bar(ctrader,EURUSD,1m,1.4)",
        ]
    );
    let closed: Vec<i64> = rec
        .recorded()
        .into_iter()
        .filter_map(|c| match c {
            vike_data::SinkCall::CloseBar { bar, .. } => Some(bar.timestamp),
            _ => None,
        })
        .collect();
    assert_eq!(closed, [t0], "the closed bar keeps its period's open");
}

/// A snapshot of an EARLIER period than the one held closes nothing and replaces nothing.
#[test]
fn an_older_periods_snapshot_closes_nothing() {
    let (rec, sink) = closes();
    let t0 = 1_000 * MINUTE;
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(t0 + MINUTE, 1.2));
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(t0, 1.1));
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(t0 + 2 * MINUTE, 1.3));
    assert_eq!(
        rec.calls(),
        [
            "forming_bar(ctrader,EURUSD,1m,1.2)",
            "forming_bar(ctrader,EURUSD,1m,1.1)",
            "close_bar(ctrader,EURUSD,1m,1.2)",
            "forming_bar(ctrader,EURUSD,1m,1.3)",
        ]
    );
}

/// **A seed is closed bars**: its still-forming period is held back and closed by the next period's first
/// snapshot; a seed left empty is not sent (it would replace the series with nothing); a month is still forming
/// on its 31st day.
#[test]
fn a_seed_holds_back_its_forming_period_and_closes_it_later() {
    let (rec, sink) = closes();
    let now = this_minute();
    sink.seed_bars(
        "ctrader",
        "EURUSD",
        "1m",
        vec![bar(now - 2 * MINUTE, 1.0), bar(now - MINUTE, 1.1), bar(now, 1.2)],
    );
    sink.seed_bars("ctrader", "EURUSD", "1h", vec![bar(now - 10 * HOUR, 2.0)]);
    sink.seed_bars("ctrader", "EURUSD", "5m", vec![bar(now, 3.0)]);
    sink.seed_bars("ctrader", "EURUSD", "1M", vec![bar(now - 30 * DAY - HOUR, 4.0)]);
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now + MINUTE, 1.3));
    assert_eq!(
        rec.calls(),
        [
            "seed_bars(ctrader,EURUSD,1m,2)",
            "seed_bars(ctrader,EURUSD,1h,1)",
            "close_bar(ctrader,EURUSD,1m,1.2)",
            "forming_bar(ctrader,EURUSD,1m,1.3)",
        ]
    );
}

/// A seed that answers after the stream's own snapshot of the same period does not replace it.
#[test]
fn a_seed_does_not_replace_a_live_snapshot() {
    let (rec, sink) = closes();
    let now = this_minute();
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now, 1.5));
    sink.seed_bars("ctrader", "EURUSD", "1m", vec![bar(now - MINUTE, 1.1), bar(now, 1.2)]);
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now + MINUTE, 1.6));
    assert_eq!(
        rec.calls(),
        [
            "forming_bar(ctrader,EURUSD,1m,1.5)",
            "seed_bars(ctrader,EURUSD,1m,1)",
            "close_bar(ctrader,EURUSD,1m,1.5)",
            "forming_bar(ctrader,EURUSD,1m,1.6)",
        ]
    );
}

/// **Nothing is closed across an outage from the snapshot before it**: the outage names where the key's hole starts
/// (the dropped snapshot's period, nothing having closed), and a key subscribed or released again forgets only itself.
#[test]
fn an_outage_or_a_released_key_forgets_what_was_held() {
    let (rec, sink) = closes();
    let t0 = 1_000 * MINUTE;
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(t0, 1.1));
    assert_eq!(sink.forget_all(), [("EURUSD".to_string(), "1m".to_string(), t0)]);
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(t0 + MINUTE, 1.2));
    sink.forming_bar("ctrader", "EURUSD", "1h", bar(t0, 2.0));
    sink.forget("EURUSD", "1m");
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(t0 + 2 * MINUTE, 1.3));
    sink.forming_bar("ctrader", "EURUSD", "1h", bar(t0 + HOUR, 2.1));
    assert_eq!(
        rec.calls(),
        [
            "forming_bar(ctrader,EURUSD,1m,1.1)",
            "forming_bar(ctrader,EURUSD,1m,1.2)",
            "forming_bar(ctrader,EURUSD,1h,2)",
            "forming_bar(ctrader,EURUSD,1m,1.3)",
            "close_bar(ctrader,EURUSD,1h,2)",
            "forming_bar(ctrader,EURUSD,1h,2.1)",
        ]
    );
}

/// **The answer for a key with a fill asked is the fill's, whatever envelope it came in**: it closes the venue's stored
/// bars from the hole on, oldest first (the hole starts just past the last bar handed on closed, here the seed's, so a
/// stored bar of that one is not closed again), its period still forming is held and closed by the next period's
/// first snapshot, and none of it is a seed. The NEXT answer for that key, with no fill asked, is a seed again.
#[test]
fn an_answer_for_a_key_with_a_fill_asked_is_the_fills_and_the_next_one_a_seed() {
    let (rec, sink) = closes();
    let now = this_minute();
    sink.seed_bars("ctrader", "EURUSD", "1m", vec![bar(now - 4 * MINUTE, 1.0)]);
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now - 3 * MINUTE, 1.1));
    let from = now - 4 * MINUTE + 1;
    assert_eq!(sink.forget_all(), [("EURUSD".to_string(), "1m".to_string(), from)]);
    sink.asked_fill("EURUSD", "1m", from);
    assert!(sink.awaits_fill("EURUSD", "1m") && !sink.awaits_fill("EURUSD", "5m"));
    let stored =
        [(4, 1.0), (3, 1.2), (2, 1.3), (0, 1.4)].map(|(ago, px)| bar(now - ago * MINUTE, px));
    sink.seed_bars("ctrader", "EURUSD", "1m", stored.to_vec());
    assert!(!sink.awaits_fill("EURUSD", "1m"), "the answer took the fill");
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now + MINUTE, 1.5));
    sink.seed_bars("ctrader", "EURUSD", "1m", stored.to_vec());
    assert_eq!(
        rec.calls(),
        [
            "seed_bars(ctrader,EURUSD,1m,1)",
            "forming_bar(ctrader,EURUSD,1m,1.1)",
            "close_bar(ctrader,EURUSD,1m,1.2)",
            "close_bar(ctrader,EURUSD,1m,1.3)",
            "close_bar(ctrader,EURUSD,1m,1.4)",
            "forming_bar(ctrader,EURUSD,1m,1.5)",
            "seed_bars(ctrader,EURUSD,1m,3)",
        ]
    );
}

/// **A key's live closes wait for its fill, and go on right after it**: from the outage on, the stream's close of a
/// later period is held back, so the fill answered after it still closes the hidden periods from the venue's stored
/// bars, oldest first, and the held close follows only where it is newer (the stored bar of a period wins over the
/// stream's). Its snapshots go on at once.
#[test]
fn a_keys_live_closes_wait_for_its_fill_and_follow_its_stored_bars() {
    let (rec, sink) = closes();
    let now = this_minute();
    sink.seed_bars("ctrader", "EURUSD", "1m", vec![bar(now - 5 * MINUTE, 1.0)]);
    let from = now - 5 * MINUTE + 1;
    assert_eq!(sink.forget_all(), [("EURUSD".to_string(), "1m".to_string(), from)]);
    sink.asked_fill("EURUSD", "1m", from);
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now - 3 * MINUTE, 1.1));
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now - 2 * MINUTE, 1.2));
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now - MINUTE, 1.3));
    let stored = [(5, 1.0), (4, 1.4), (3, 1.5)].map(|(ago, px)| bar(now - ago * MINUTE, px));
    sink.seed_bars("ctrader", "EURUSD", "1m", stored.to_vec());
    assert!(!sink.awaits_fill("EURUSD", "1m"));
    assert_eq!(
        rec.calls(),
        [
            "seed_bars(ctrader,EURUSD,1m,1)",
            "forming_bar(ctrader,EURUSD,1m,1.1)",
            "forming_bar(ctrader,EURUSD,1m,1.2)",
            "forming_bar(ctrader,EURUSD,1m,1.3)",
            "close_bar(ctrader,EURUSD,1m,1.4)",
            "close_bar(ctrader,EURUSD,1m,1.5)",
            "close_bar(ctrader,EURUSD,1m,1.2)",
        ]
    );
}

/// **A fill whose wait was given up, answered after the stream closed a later period, closes nothing, and is still no
/// seed**: giving up lets the held closes go on, the core drops a close older than its last, so the hidden period stays a
/// hole (a WARN). A refused fill lets them go on too. A key subscribed or released again, and a socket that dies again,
/// drop the fill asked, and the answer is then a seed.
#[test]
fn a_fill_answered_after_a_later_close_closes_nothing_and_a_forgotten_fill_is_a_seed() {
    let (rec, sink) = closes();
    let now = this_minute();
    sink.seed_bars("ctrader", "EURUSD", "1m", vec![bar(now - 4 * MINUTE, 1.0)]);
    let from = now - 4 * MINUTE + 1;
    assert_eq!(sink.forget_all(), [("EURUSD".to_string(), "1m".to_string(), from)]);
    sink.asked_fill("EURUSD", "1m", from);
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now - 2 * MINUTE, 1.1));
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now - MINUTE, 1.2));
    sink.gave_up_fill("EURUSD", "1m");
    assert!(sink.awaits_fill("EURUSD", "1m"), "a late answer is still the fill's");
    let stored = [(4, 1.0), (3, 1.3), (2, 1.4)].map(|(ago, px)| bar(now - ago * MINUTE, px));
    sink.seed_bars("ctrader", "EURUSD", "1m", stored.to_vec());
    assert!(!sink.awaits_fill("EURUSD", "1m"));

    sink.forget_all();
    sink.asked_fill("EURUSD", "1m", from);
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now, 1.5));
    sink.forming_bar("ctrader", "EURUSD", "1m", bar(now + MINUTE, 1.6));
    sink.refused_fill("EURUSD", "1m");
    assert!(!sink.awaits_fill("EURUSD", "1m"), "a refused fill is dropped");

    sink.asked_fill("EURUSD", "1m", from);
    sink.forget("EURUSD", "1m");
    sink.seed_bars("ctrader", "EURUSD", "1m", stored.to_vec());
    sink.asked_fill("EURUSD", "1m", from);
    sink.forget_all();
    sink.seed_bars("ctrader", "EURUSD", "1m", stored.to_vec());
    assert_eq!(
        rec.calls(),
        [
            "seed_bars(ctrader,EURUSD,1m,1)",
            "forming_bar(ctrader,EURUSD,1m,1.1)",
            "forming_bar(ctrader,EURUSD,1m,1.2)",
            "close_bar(ctrader,EURUSD,1m,1.1)",
            "forming_bar(ctrader,EURUSD,1m,1.5)",
            "forming_bar(ctrader,EURUSD,1m,1.6)",
            "close_bar(ctrader,EURUSD,1m,1.5)",
            "seed_bars(ctrader,EURUSD,1m,3)",
            "seed_bars(ctrader,EURUSD,1m,3)",
        ]
    );
}
