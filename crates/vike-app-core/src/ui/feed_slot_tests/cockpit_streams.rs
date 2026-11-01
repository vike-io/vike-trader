//! `reap_orphaned_trade_cockpit_streams`: the depth/cockpit teardown and its retry sweep.

use super::*;
use crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN;

// ----- reap_orphaned_trade_cockpit_streams --------------------------------------------------

/// **The B1-flagged leak, closed.** Deleting the last Trade window on a book stops exactly
/// that book's stream — its depth leg, unsubscribed on the entry's OWN venue — while another book's
/// live stream is untouched.
#[test]
fn deleting_a_trade_window_unsubscribes_exactly_its_depth_streams() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT"); // depth id 101, trades 102
    env.depth("okx", "ETH-USDT-SWAP"); // depth id 201, trades 202
    assert_eq!(env.calls().len(), 4, "each window's depth and its tape");

    env.reap_streams(&[trade_win("binance", "BTCUSDT")]); // the ETH-USDT-SWAP window is gone
    assert_eq!(
        env.calls()[4..].to_vec(),
        vec!["okx:unsub 201".to_string(), "okx:unsub 202".to_string()],
        "the ETH-USDT-SWAP streams stopped on OKX; the live BTCUSDT ones are untouched"
    );
    assert!(env.trade_depth.contains_key(&("binance".to_string(), "BTCUSDT".to_string())));
    assert!(!env.trade_depth.contains_key(&("okx".to_string(), "ETH-USDT-SWAP".to_string())));
}

/// A merely-closed (`open = false`) Trade window keeps its streams — the same rule the kline
/// reaper applies ([`orphaned_feed_keys`]'s "regardless of `open`/`minimized`" note): close
/// hides the window off-desktop and the rail can unhide it; only DELETION tears down.
#[test]
fn a_closed_trade_window_keeps_its_depth_stream() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT");
    let mut w = trade_win("binance", "BTCUSDT");
    w.open = false;
    env.reap_streams(std::slice::from_ref(&w));
    assert_eq!(env.calls().len(), 2, "the depth and the tape, and no unsubscribe");
    assert!(env.trade_depth.contains_key(&("binance".to_string(), "BTCUSDT".to_string())));
}

/// A Trade window names its venue in its `WinState`, so the reaper is venue-SPECIFIC: moving the
/// window to another venue releases the old venue's stream on the next reap. (The DOM's reaper was
/// venue-BLIND because the DOM kept its venue in its tool view, where `wins` could not see it.)
#[test]
fn a_trade_window_that_moves_venue_releases_the_old_venues_stream() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT"); // depth id 101, trades 102
    env.depth("okx", "BTC-USDT-SWAP"); // depth id 201, trades 202
    env.reap_streams(&[trade_win("okx", "BTC-USDT-SWAP")]);
    assert_eq!(
        env.calls()[4..].to_vec(),
        vec!["binance:unsub 101".to_string(), "binance:unsub 102".to_string()],
        "the window moved to OKX, so the Binance book is released"
    );
    assert_eq!(env.trade_depth.len(), 1);
    assert!(env.trade_depth.contains_key(&("okx".to_string(), "BTC-USDT-SWAP".to_string())));
}

/// **The reaper matches `(venue, native symbol)` TOGETHER, and counts EVERY Trade window.**
///
/// The test above cannot tell a reaper that matches the whole key from one that matches the symbol
/// alone (the DOM's old matching): its two books have different native symbols. Binance, Bybit and
/// Aster all trade `BTCUSDT`, so here the SAME native symbol lives on two venues. Two Trade
/// windows, one per venue, keep BOTH streams — and a reaper that counted only the first (or only
/// the last) Trade window would release one of them. With only the Bybit window left, exactly
/// Binance's stream goes, even though Bybit's window names the same symbol.
#[test]
fn the_reaper_matches_venue_and_symbol_together_and_counts_every_trade_window() {
    let mut env = Env::with_feeds(&[("binance", 100, &[]), ("bybit", 300, &[])]);
    env.depth("binance", "BTCUSDT"); // depth id 101, trades 102
    env.depth("bybit", "BTCUSDT"); // depth id 301, trades 302
    assert_eq!(env.calls().len(), 4);

    env.reap_streams(&[trade_win("binance", "BTCUSDT"), trade_win("bybit", "BTCUSDT")]);
    assert_eq!(env.calls().len(), 4, "two windows, two books: nothing is released");
    assert_eq!(env.trade_depth.len(), 2);

    env.reap_streams(&[trade_win("bybit", "BTCUSDT")]);
    assert_eq!(
        env.calls()[4..].to_vec(),
        vec!["binance:unsub 101".to_string(), "binance:unsub 102".to_string()],
        "the Binance window is gone; the Bybit window names the SAME symbol but not the same book"
    );
    assert!(!env.trade_depth.contains_key(&("binance".to_string(), "BTCUSDT".to_string())));
    assert!(env.trade_depth.contains_key(&("bybit".to_string(), "BTCUSDT".to_string())));
}

/// Teardown then re-open genuinely restarts the stream — a FRESH subscription id, the DOM
/// twin of `teardown_then_re_add_genuinely_restarts_the_feed`. The idempotency entry must
/// have been freed, or a re-opened Trade window would paint a book nothing feeds.
#[test]
fn teardown_then_reopen_genuinely_restarts_the_depth_stream() {
    let mut env = Env::new();
    let key = ("binance".to_string(), "BTCUSDT".to_string());
    env.depth("binance", "BTCUSDT");
    assert_eq!(env.trade_depth[&key].depth, SubscriptionId(101));

    env.reap_streams(&[]);
    assert!(env.trade_depth.is_empty());

    env.depth("binance", "BTCUSDT");
    env.depth("binance", "BTCUSDT"); // still idempotent after the round trip
    assert_eq!(
        env.calls(),
        vec![
            "binance:depth BTCUSDT".to_string(),
            "binance:trades BTCUSDT".to_string(),
            "binance:unsub 101".to_string(),
            "binance:unsub 102".to_string(),
            "binance:depth BTCUSDT".to_string(),
            "binance:trades BTCUSDT".to_string(),
        ],
        "a real second subscribe — and exactly one"
    );
    assert_eq!(env.trade_depth[&key].depth, SubscriptionId(103), "a FRESH id");
}

/// Teardown with the venue's client gone (a de-registered venue; every `--observe` session)
/// is tolerated: the entry is dropped with no unsubscribe and no panic, and a later re-reap
/// finds nothing left to stop — an id is never unsubscribed twice.
#[test]
fn teardown_without_the_venues_client_is_tolerated_and_never_double_unsubscribes() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT");
    env.poly("tok-1"); // no polymarket feed registered -> a retry record, no entry

    env.feeds.remove("binance");
    env.reap_streams(&[]);
    assert!(env.trade_depth.is_empty() && env.poly_subs.is_empty());
    assert!(!env.calls().iter().any(|c| c.contains("unsub")), "no client, no unsubscribe");

    // Re-register the venue and re-reap: the torn-down entry stays down.
    let log = Arc::clone(&env.log);
    env.feeds.insert("binance", FakeFeed::boxed("binance", &log, 500, &[]));
    env.reap_streams(&[]);
    assert!(!env.calls().iter().any(|c| c.contains("unsub")), "nothing left to stop");
}

/// An unsubscribe for an id the CLIENT no longer knows is tolerated — the venue client was
/// replaced mid-session (a reconnect), so the teardown's id was never issued by the client
/// that receives it. [`DataClient::unsubscribe`]'s contract pins unknown ids as a no-op
/// ("never panics"); this proves the teardown path leans on exactly that and nothing more.
#[test]
fn an_unsubscribe_for_an_id_the_client_no_longer_knows_is_tolerated() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT"); // id 101, minted by the ORIGINAL client
    let log = Arc::clone(&env.log);
    env.feeds.insert("binance", FakeFeed::boxed("binance", &log, 500, &[]));
    env.reap_streams(&[]); // must not panic
    assert_eq!(
        env.calls().last().unwrap(),
        "binance:unsub 102",
        "the tape's leg, after the depth's"
    );
    assert!(env.trade_depth.is_empty());
}

/// The cockpit twin of the DOM teardown: deleting the last window on a token stops its book
/// AND trade streams on the polymarket feed, another token's cockpit is untouched, and a
/// re-opened window genuinely resubscribes with fresh ids.
#[test]
fn deleting_a_cockpit_window_unsubscribes_its_book_and_trade_streams() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
    env.poly("tok-1"); // book 401 + trades 402
    env.poly("tok-2"); // book 403 + trades 404

    env.reap_streams(&[poly_win("tok-1")]); // tok-2's window is gone
    assert_eq!(
        env.calls()[4..].to_vec(),
        vec!["polymarket:unsub 403".to_string(), "polymarket:unsub 404".to_string()],
        "both tok-2 legs stopped; the live tok-1 streams are untouched"
    );
    assert!(env.poly_subs.contains_key("tok-1") && !env.poly_subs.contains_key("tok-2"));

    env.reap_streams(&[]);
    assert!(env.poly_subs.is_empty());

    env.poly("tok-1");
    assert_eq!(
        env.calls()[8..].to_vec(),
        vec!["polymarket:book tok-1".to_string(), "polymarket:trades tok-1".to_string()],
        "a re-opened cockpit genuinely resubscribes"
    );
    assert_eq!(env.poly_subs["tok-1"].book, SubscriptionId(405), "a FRESH id");
}

/// A still-resolving cockpit window (placeholder token) holds nothing alive: it never
/// subscribed anything ([`ensure_poly_book`] refuses the placeholder), so it cannot pin
/// another token's stream either.
#[test]
fn a_placeholder_cockpit_window_pins_no_stream() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
    env.poly("tok-1");
    env.reap_streams(&[poly_win(POLY_PLACEHOLDER_TOKEN)]);
    assert!(env.poly_subs.is_empty(), "tok-1's window is gone; the placeholder is not it");
}

/// The lane sweep — [`reap_orphaned_feeds`]'s step (2e), applied to the depth/cockpit lanes
/// this teardown owns: a torn-down entry's failing SECOND-leg record (the cockpit's trade tape)
/// goes with it (the retry arm lives behind the entry, so nothing could ever re-attempt it), while
/// a live window's record stays (the venue is still saying no, so the report is still true).
#[test]
fn the_stream_reaper_sweeps_the_trade_depth_and_cockpit_retry_lanes() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &["trades"])]);
    env.poly("tok-1"); // book live, trade leg failing
    let trades_key = RetryKey::poly_trades("tok-1");
    env.reap_streams(&[poly_win("tok-1")]);
    assert!(env.retries.is_pending(&trades_key), "window live: the record is still true");

    env.reap_streams(&[]);
    assert!(!env.retries.is_pending(&trades_key), "torn down: nothing can re-attempt the leg");
    assert!(env.retries.is_empty());
}

/// The entry-LESS half of that sweep: a failing depth/book subscribe records no entry (that
/// is what keeps it retryable), so when its window is deleted only the retry record remains —
/// with no per-frame `ensure_*` caller left, it could only sit idle forever. It is swept;
/// a still-open window's record survives.
#[test]
fn the_stream_reaper_sweeps_failing_subscribes_whose_window_is_gone() {
    let mut env = Env::with_feeds(&[("binance", 100, &["depth"]), ("polymarket", 400, &["book"])]);
    env.depth("binance", "BTCUSDT");
    env.poly("tok-1");
    assert_eq!(env.retries.len(), 2);

    env.reap_streams(&[trade_win("binance", "BTCUSDT"), poly_win("tok-1")]);
    assert_eq!(env.retries.len(), 2, "both windows live: both records stay");

    env.reap_streams(&[trade_win("binance", "BTCUSDT")]);
    assert!(env.retries.is_pending(&RetryKey::trade_depth("binance", "BTCUSDT")));
    assert!(!env.retries.is_pending(&RetryKey::poly_book("tok-1")), "cockpit gone");

    env.reap_streams(&[]);
    assert!(env.retries.is_empty());
}

/// The retry-lane twin of the reaper test above that matches venue and symbol together.
/// A failing depth subscribe records no entry, only a `RetryLane::TradeDepth` record keyed
/// `"{venue}:{inst}"`, and the lane sweep judges THAT against the Trade windows' `(venue, symbol)`
/// pairs too: with Binance and Bybit both failing on `BTCUSDT`, deleting the Binance window drops
/// Binance's record and leaves Bybit's, whose window names the same symbol.
#[test]
fn the_stream_reaper_sweeps_a_failing_depth_record_by_venue_and_symbol_together() {
    let mut env = Env::with_feeds(&[("binance", 100, &["depth"]), ("bybit", 300, &["depth"])]);
    env.depth("binance", "BTCUSDT");
    env.depth("bybit", "BTCUSDT");
    assert!(env.trade_depth.is_empty(), "both subscribes failed: records only, no entries");
    assert_eq!(env.retries.len(), 2);

    env.reap_streams(&[trade_win("binance", "BTCUSDT"), trade_win("bybit", "BTCUSDT")]);
    assert_eq!(env.retries.len(), 2, "two windows live: both records stay");

    env.reap_streams(&[trade_win("bybit", "BTCUSDT")]);
    assert!(env.retries.is_pending(&RetryKey::trade_depth("bybit", "BTCUSDT")));
    assert!(
        !env.retries.is_pending(&RetryKey::trade_depth("binance", "BTCUSDT")),
        "the Binance window is gone, so its record goes — whatever symbol Bybit's window names"
    );
    assert_eq!(env.retries.len(), 1);
}
