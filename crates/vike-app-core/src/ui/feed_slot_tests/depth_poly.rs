//! `ensure_depth` and `ensure_poly_book`: the Trade window's and the cockpit's streams.

use super::*;
use crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN;

// ----- ensure_depth -----------------------------------------------------------------------

/// Binance: the depth stream and the print stream the tick chart paints from. Idempotent via
/// `trade_depth`, so the per-frame call never opens a second stream of either.
#[test]
fn ensure_depth_on_binance_subscribes_depth_and_the_tape_and_dedups() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT");
    env.depth("binance", "BTCUSDT");
    assert_eq!(
        env.calls(),
        vec!["binance:depth BTCUSDT".to_string(), "binance:trades BTCUSDT".to_string()]
    );
    let sub = &env.trade_depth[&("binance".to_string(), "BTCUSDT".to_string())];
    assert_eq!(sub.depth, SubscriptionId(101), "the minted id is KEPT (not dropped on the floor)");
    assert_eq!(sub.trades, Some(SubscriptionId(102)), "...and the tape's, for the reaper");
    assert!(env.retries.is_empty(), "the healthy path records nothing");
}

/// M-4 of the Trade window's final review (Ruling R6): a Trade window opens NO bar feed, on any
/// venue. The DOM's non-Binance `"1m"` leg ("so that venue's PAPER engine fills") fed an engine the
/// desktop no longer runs, the datahub feed refuses the bar lane, and each instrument a window
/// opened logged one warn for it. On OKX's dashed SWAP-perp inst — the venue-native symbol the
/// Trade window trades, so nothing is translated — the streams are the depth and the tape, and no
/// bars.
#[test]
fn ensure_depth_opens_no_bar_feed_on_any_venue() {
    let mut env = Env::new();
    env.depth("okx", "BTC-USDT-SWAP");
    env.depth("okx", "BTC-USDT-SWAP");
    assert_eq!(
        env.calls(),
        vec!["okx:depth BTC-USDT-SWAP".to_string(), "okx:trades BTC-USDT-SWAP".to_string()]
    );
    let sub = &env.trade_depth[&("okx".to_string(), "BTC-USDT-SWAP".to_string())];
    assert_eq!(sub.depth, SubscriptionId(201), "the entry keeps what teardown needs");
    assert!(env.retries.is_empty());
}

/// A FAILED depth subscribe still records no `trade_depth` entry — that is what keeps it
/// retryable at all — but the retry is now THROTTLED rather than fired on the very next frame:
/// `ensure_depth` is a per-frame call, so the old shape re-dialled a live venue socket (and
/// wrote a `warn!`) ~60×/second for as long as the Trade window stayed open.
#[test]
fn a_failed_depth_subscribe_is_retried_after_its_cooldown_not_next_frame() {
    let mut env = Env::with_feeds(&[("binance", 100, &["depth"])]);
    env.depth("binance", "BTCUSDT");
    assert!(env.trade_depth.is_empty(), "nothing recorded, so it stays retryable");
    for _ in 0..10 {
        env.depth("binance", "BTCUSDT");
    }
    assert_eq!(env.calls().len(), 1, "no ~60 Hz re-dial while the cooldown holds");

    env.elapse();
    env.depth("binance", "BTCUSDT");
    assert_eq!(env.calls().len(), 2, "the retry really happens, one per elapsed cooldown");
}

/// An unregistered venue is logged and skipped — never a panic, never a `trade_depth` entry.
/// The miss is now RECORDED instead of re-warned on every frame (the same warn-once treatment
/// `unroutable` gives the series lane), and stays immediately retryable because re-checking a
/// hashmap costs nothing.
#[test]
fn a_missing_depth_feed_is_recorded_once_and_stays_immediately_retryable() {
    let mut env = Env::with_feeds(&[]);
    env.depth("bybit", "BTCUSDT");
    assert!(env.calls().is_empty() && env.trade_depth.is_empty());
    let key = RetryKey::trade_depth("bybit", "BTCUSDT");
    assert!(env.retries.is_pending(&key), "the miss is recorded, so the warn is once");
    assert!(env.retries.is_retry_due(&key), "…and free to re-check every frame");

    // Register the venue: the very next call subscribes, with no cooldown to wait out.
    let log = Arc::clone(&env.log);
    env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
    env.depth("bybit", "BTCUSDT");
    assert_eq!(env.calls()[0], "bybit:depth BTCUSDT");
    assert!(env.trade_depth.contains_key(&("bybit".to_string(), "BTCUSDT".to_string())));
}

// ----- ensure_poly_book -------------------------------------------------------------------

/// The happy path: book + trades on the polymarket feed, token recorded, then deduped.
#[test]
fn ensure_poly_book_subscribes_book_and_trades_once() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
    env.poly("tok-1");
    env.poly("tok-1");
    assert_eq!(
        env.calls(),
        vec!["polymarket:book tok-1".to_string(), "polymarket:trades tok-1".to_string()]
    );
    assert!(env.poly_subs.contains_key("tok-1"));
    assert_eq!(
        (env.poly_subs["tok-1"].book, env.poly_subs["tok-1"].trades),
        (SubscriptionId(401), Some(SubscriptionId(402))),
        "both minted ids are KEPT for teardown"
    );
    assert!(env.retries.is_empty(), "the healthy path records nothing");
}

/// An empty or placeholder token is skipped BEFORE any feed lookup — a cockpit window whose
/// token was never seeded must never subscribe the literal placeholder id.
#[test]
fn an_empty_or_placeholder_token_never_subscribes() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
    env.poly("");
    env.poly(POLY_PLACEHOLDER_TOKEN);
    assert!(env.calls().is_empty() && env.poly_subs.is_empty());
    assert!(env.retries.is_empty(), "a skipped token is not a failure");
}

/// A failed BOOK subscribe records no `poly_subs` entry (so it stays retryable), but is now
/// throttled rather than re-dialled on every frame.
#[test]
fn a_failed_poly_book_subscribe_is_retried_after_its_cooldown() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &["book"])]);
    env.poly("tok-1");
    assert!(env.poly_subs.is_empty(), "book failed -> still retryable");
    for _ in 0..10 {
        env.poly("tok-1");
    }
    assert_eq!(env.calls().len(), 1, "no ~60 Hz re-dial while the cooldown holds");
    env.elapse();
    env.poly("tok-1");
    assert_eq!(env.calls().len(), 2, "the retry really happens");
}

/// **The `poly_subs` burn.** A failed TRADES subscribe still records the token — the book is
/// live and the ladder paints, so re-running `subscribe_book` every frame would be the other
/// bug — but that record used to suppress every later call, leaving the token's trade tape
/// permanently absent behind one `warn!`. The trade leg now carries its own record and is
/// re-attempted ALONE, without touching the live book.
#[test]
fn a_failed_poly_trades_leg_is_retried_without_resubscribing_the_live_book() {
    let mut env = Env::with_feeds(&[]);
    let log = Arc::clone(&env.log);
    let fail = fail_set(&["trades"]);
    env.feeds.insert("polymarket", FakeFeed::boxed_flaky("polymarket", &log, 400, &fail));

    env.poly("tok-1");
    assert!(env.poly_subs.contains_key("tok-1"), "book is live, so the token is recorded");
    let trades_key = RetryKey::poly_trades("tok-1");
    assert!(env.retries.is_pending(&trades_key), "the missing tape is recorded, not dropped");

    for _ in 0..10 {
        env.poly("tok-1");
    }
    assert_eq!(env.calls().len(), 2, "throttled, and the live book is left alone");

    fail.lock().unwrap().clear();
    env.elapse();
    env.poly("tok-1");
    assert_eq!(
        env.calls(),
        vec![
            "polymarket:book tok-1".to_string(),
            "polymarket:trades tok-1".to_string(),
            "polymarket:trades tok-1".to_string(),
        ],
        "ONLY the trade leg is re-attempted — the book is never re-subscribed"
    );
    assert!(env.retries.is_empty(), "recovered: prints resume");
    assert_eq!(
        env.poly_subs["tok-1"].trades,
        Some(SubscriptionId(402)),
        "the recovered leg's id lands on the entry, so teardown can stop it too"
    );

    env.poly("tok-1");
    assert_eq!(env.calls().len(), 3, "and it is once, not once per frame");
}

/// No polymarket feed registered (the default/CI build, where the bridge is feature-gated
/// away): logged and skipped, never a panic — and, like the DOM twin, recorded so the warn is
/// once per token rather than once per frame.
#[test]
fn ensure_poly_book_without_a_polymarket_feed_is_a_no_op() {
    let mut env = Env::new();
    env.poly("tok-1");
    assert!(env.calls().is_empty() && env.poly_subs.is_empty());
    let key = RetryKey::poly_book("tok-1");
    assert!(env.retries.is_pending(&key) && env.retries.is_retry_due(&key));
}
