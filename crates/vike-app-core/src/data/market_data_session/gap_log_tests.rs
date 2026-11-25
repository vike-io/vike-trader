//! **What the log says about a stream's health** (the owner's console, 2026-11-16): the frames a Trade
//! window's subscriptions produce, applied through `apply_frame` as the reader applies them, and the
//! WARNs and `live again` INFOs that come out of the session.

use std::sync::Arc;

use vike_datahub_client::BookSnapshot;
use vike_datahub_client::market::MarketDataSpec;
use vike_log::capture::{CapturedEvent, captured};
use vike_marketdata::L2BookLevel;
use vike_model::{StreamLabel, StreamStatus};

use super::connection::MarketDataSessionReaderState;
use super::*;

fn session() -> (MarketDataSessionMount, tempfile::TempDir) {
    let no_store = tempfile::tempdir().expect("a scratch settings directory");
    let mount = build(
        no_store.path().to_str(),
        Arc::new(L2BookStore::default()),
        Arc::new(TickStore::default()),
        || {},
    );
    (mount, no_store)
}

fn spec(venue: &str) -> MarketDataSpec {
    MarketDataSpec {
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        lane: StreamLabel::Depth,
        depth_levels: None,
    }
}

/// A Trade window's depth subscription, as `DataClientDatahub` makes it.
fn want(s: &MarketDataSession, venue: &'static str) {
    s.want(venue, "BTCUSDT", StreamLabel::Depth).expect("the depth lane is wanted");
}

fn depth(venue: &str, sequence: u64) -> MarketDataFrame {
    MarketDataFrame::Depth(BookSnapshot {
        venue: venue.to_string(),
        symbol: "BTCUSDT".to_string(),
        tick_size: 0.1,
        bids: vec![L2BookLevel::new(65_000.0, 1.0)],
        asks: vec![L2BookLevel::new(65_000.1, 1.0)],
        venue_timestamp: 1,
        venue_sequence: sequence,
        sequence,
    })
}

fn status(venue: &str, status: StreamStatus) -> MarketDataFrame {
    MarketDataFrame::Status {
        venue: venue.to_string(),
        symbol: "BTCUSDT".to_string(),
        lane: StreamLabel::Depth,
        status,
    }
}

const GAP: StreamStatus = StreamStatus::GapStart { at_timestamp_ms: 1_000 };
const STALE: StreamStatus = StreamStatus::Stale { newest_data_timestamp_ms: 0, now_ms: 2_000 };
const NO_DATA_YET: &str = "market-data stream subscribed, no data yet";

/// The market-data WARNs and the `live again` INFOs among `events`.
fn gaps(events: &[CapturedEvent]) -> (usize, usize) {
    let warns = events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN && e.message.starts_with("market-data"))
        .count();
    let lives =
        events.iter().filter(|e| e.message.starts_with("market-data stream live again")).count();
    (warns, lives)
}

/// **A Trade window opened on binance, switched to bybit and back**: each newly wanted key is handed
/// the hub's attach `GapStart` ("wanted, not yet live") and then its book, and the old key is released
/// with a frame of it still in flight. The owner's log showed a WARN for each new key and no `live
/// again` after it; there is neither now, also for a key wanted again on the same connection.
#[test]
fn opening_and_switching_a_trade_window_logs_no_gap() {
    let (mount, _dir) = session();
    let s = &mount.session;
    let mut st = MarketDataSessionReaderState::default();
    let ((), events) = captured(|| {
        want(s, "binance");
        s.apply_frame(status("binance", GAP), &mut st);
        s.apply_frame(depth("binance", 1), &mut st);
        s.apply_frame(depth("binance", 2), &mut st);
        // The switch: the old key released, the new one attached and flowing.
        s.unwant(&spec("binance"));
        s.apply_frame(depth("binance", 3), &mut st);
        want(s, "bybit");
        s.apply_frame(status("bybit", GAP), &mut st);
        s.apply_frame(depth("bybit", 1), &mut st);
        // An attach `Live` for a key already flowing at the hub is no recovery either.
        s.apply_frame(
            status("bybit", StreamStatus::Live { gap_started_timestamp_ms: None }),
            &mut st,
        );
        // And back: the first key wanted again starts afresh.
        s.unwant(&spec("bybit"));
        want(s, "binance");
        s.apply_frame(status("binance", GAP), &mut st);
        s.apply_frame(depth("binance", 4), &mut st);
    });
    assert_eq!(gaps(&events), (0, 0), "{events:#?}");
}

/// **A real stall then its resume**: the venue's book stops (a `GapStart` on a key that has flowed),
/// then flows again with no `Live` from the server, as a healthy feed sends none. Exactly one WARN,
/// then exactly one INFO, and a repeated `GapStart` inside the same gap adds nothing.
#[test]
fn a_real_stall_then_its_resume_logs_one_warn_then_one_info() {
    let (mount, _dir) = session();
    let s = &mount.session;
    want(s, "bybit");
    want(s, "binance");
    let mut st = MarketDataSessionReaderState::default();
    let ((), events) = captured(|| {
        s.apply_frame(depth("bybit", 1), &mut st);
        s.apply_frame(status("bybit", GAP), &mut st);
        s.apply_frame(status("bybit", GAP), &mut st);
        s.apply_frame(depth("bybit", 2), &mut st);
        s.apply_frame(depth("bybit", 3), &mut st);
    });
    assert_eq!(gaps(&events), (1, 1), "{events:#?}");
    let warn = events.iter().position(|e| e.message.starts_with("market-data gap started"));
    let info = events.iter().position(|e| e.message.starts_with("market-data stream live again"));
    assert!(warn < info, "the WARN comes first: {events:#?}");

    // The same with the server's own `Live`: still one INFO, not one for the `Live` and one for the data.
    let ((), events) = captured(|| {
        s.apply_frame(depth("binance", 1), &mut st);
        s.apply_frame(status("binance", GAP), &mut st);
        s.apply_frame(
            status("binance", StreamStatus::Live { gap_started_timestamp_ms: Some(1_000) }),
            &mut st,
        );
        s.apply_frame(depth("binance", 2), &mut st);
    });
    assert_eq!(gaps(&events), (1, 1), "{events:#?}");
}

/// **A wanted key that never delivers is not silent**: past the hub's attach `GapStart`, the server's
/// own `Stale` (or a second `GapStart`) on a key that has carried nothing is one WARN, `subscribed,
/// no data yet`; its first data, or the server's `Live`, is the one `live again`.
#[test]
fn a_wanted_key_that_never_delivers_warns_once_and_its_first_data_says_live_again() {
    let (mount, _dir) = session();
    let s = &mount.session;
    want(s, "bybit");
    want(s, "binance");
    let mut st = MarketDataSessionReaderState::default();
    let ((), events) = captured(|| {
        s.apply_frame(status("bybit", GAP), &mut st);
        s.apply_frame(status("bybit", STALE), &mut st);
        s.apply_frame(status("bybit", STALE), &mut st);
    });
    assert_eq!(gaps(&events), (1, 0), "{events:#?}");
    let warn = events.iter().find(|e| e.level == tracing::Level::WARN).expect("the WARN");
    assert!(warn.message.starts_with(NO_DATA_YET), "{warn:#?}");
    let ((), events) = captured(|| {
        s.apply_frame(depth("bybit", 1), &mut st);
        s.apply_frame(depth("bybit", 2), &mut st);
    });
    assert_eq!(gaps(&events), (0, 1), "{events:#?}");

    // A second `GapStart` says the same; the server's `Live` closes it.
    let ((), events) = captured(|| {
        s.apply_frame(status("binance", GAP), &mut st);
        s.apply_frame(status("binance", GAP), &mut st);
        s.apply_frame(
            status("binance", StreamStatus::Live { gap_started_timestamp_ms: Some(1_000) }),
            &mut st,
        );
        s.apply_frame(depth("binance", 1), &mut st);
    });
    assert_eq!(gaps(&events), (1, 1), "{events:#?}");
}

/// **A lost connection is a real gap, and the next connection's first data ends it**: the reader's
/// bracket WARNs for the key it served; the new connection's attach `GapStart` adds nothing; its first
/// book is the one `live again`.
#[test]
fn a_lost_connection_warns_once_and_the_next_one_says_live_again() {
    let (mount, _dir) = session();
    let s = &mount.session;
    want(s, "bybit");
    *lock(&s.served) = vec![spec("bybit")];
    let mut st = MarketDataSessionReaderState::default();
    let ((), events) = captured(|| {
        s.apply_frame(depth("bybit", 1), &mut st);
        s.open_gap_bracket("the link dropped");
        let mut next = MarketDataSessionReaderState::default();
        s.apply_frame(status("bybit", GAP), &mut next);
        s.apply_frame(depth("bybit", 1), &mut next);
    });
    assert_eq!(gaps(&events), (1, 1), "{events:#?}");
}
