//! The Trade window's depth stream and the cockpit's Polymarket book + trade streams.

use super::*;
use crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN;
use vike_data::SubscriptionId;

/// What one `trade_depth` entry OWNS: the live subscription id [`ensure_depth`] minted for its
/// `(venue, native symbol)` key. Kept — instead of the id being dropped on the floor, which is what
/// this map's `HashSet` predecessor did — so [`reap_orphaned_trade_cockpit_streams`] can stop
/// exactly this stream when the last Trade window on the book is deleted, and `teardown_feed_plane`
/// (the backend-switch clear's feed-plane complement) can stop it against the client that issued it
/// before forgetting it. (A backend SWITCH keeps these streams — venue truth, split-plane B2.)
///
/// ⚠ It held a second id until the Trade window's final review (M-4): the DOM's non-Binance `"1m"`
/// `subscribe_bars` leg, opened "so that venue's PAPER engine fills". The desktop has run no engine
/// since it lost its local core, the datahub feed refuses the bar lane (`Unsupported`), and the leg
/// logged one warn per instrument a Trade window opened — so it is deleted with its retry lane, not
/// kept as a second subscription that feeds nothing (Ruling R6: the Trade window opens no bar feed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeDepthSubs {
    /// `subscribe_depth`'s id — the handle `unsubscribe` needs to stop the L2 stream.
    pub depth: SubscriptionId,
    /// `subscribe_trades`' id, once it has succeeded: the print stream the window's tick chart paints
    /// bubbles from. `None` for a venue that serves no trade stream, which costs the chart its Trades
    /// layer and nothing else — the depth leg above never waits on it.
    pub trades: Option<SubscriptionId>,
}

/// What one `poly_subs` entry OWNS — [`TradeDepthSubs`]'s cockpit twin, for the same two spenders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolyBookSubs {
    /// `subscribe_book`'s id — the leg whose success records the entry (the ladder paints).
    pub book: SubscriptionId,
    /// The `subscribe_trades` leg's id, once it has succeeded; `None` while it is still
    /// failing/retrying under its [`RetryLane::PolyTrades`] record.
    pub trades: Option<SubscriptionId>,
}

/// Start a live L2 depth stream for `(venue, inst)` (once) so the Trade window renders that
/// venue's real book. The key is the window's venue and the venue-NATIVE symbol it trades, so
/// there is nothing to translate. Routes to the venue's feed; dedups via `trade_depth`, whose entry
/// KEEPS the minted subscription id ([`TradeDepthSubs`]); the stream is stopped by
/// [`reap_orphaned_trade_cockpit_streams`] once no Trade window names the book anymore (before that
/// existed, only the feed's `shutdown()` on exit ever stopped it). Called every frame for the
/// window's book — idempotent. A Trade window that moves to another venue releases the old
/// venue's stream on the next reap ([`reap_orphaned_trade_cockpit_streams`] is venue-specific).
///
/// The depth stream is the ONLY one: a Trade window opens no bar feed on any venue (Ruling R6;
/// [`TradeDepthSubs`] says what the deleted 1m leg was).
///
/// A failed depth subscribe records no `trade_depth` entry, so it stays retryable — but it used to
/// be retried on **every frame**, i.e. a ~60 Hz socket-and-log loop against a live venue. It is
/// throttled by the [`FeedRetries`] schedule, and the missing-venue arm's `warn!`, which was per
/// frame, is once per `(venue, symbol)`.
pub fn ensure_depth(
    feeds: &mut FeedMap,
    trade_depth: &mut HashMap<(String, String), TradeDepthSubs>,
    retries: &mut FeedRetries,
    venue: &str,
    inst: &str,
) {
    let key = (venue.to_string(), inst.to_string());
    // The depth stream is live and must NOT be re-subscribed.
    if trade_depth.contains_key(&key) {
        return;
    }
    let depth_key = RetryKey::trade_depth(venue, inst);
    // A previously-failed depth subscribe is retried, but only once its cooldown has elapsed —
    // this is the per-frame resubscribe loop the old code ran.
    if retries.is_blocked(&depth_key) {
        return;
    }
    let Some(feed) = feeds.get_mut(venue) else {
        if retries.note_missing(&depth_key) {
            tracing::warn!(
                "ensure_depth({venue}, {inst}): no feed registered for venue — the Trade window's \
                 ladder will stay STALE; will retry once one is registered"
            );
        }
        return;
    };
    match feed.subscribe_depth(inst) {
        Ok(depth) => {
            if let Some(n) = retries.clear(&depth_key) {
                tracing::info!(
                    "ensure_depth({venue}, {inst}): subscribe_depth recovered after {n} failed \
                     attempt(s)"
                );
            }
            // The tick chart's print stream rides the same entry. A venue with no trade lane (or a
            // session that refuses it) is not an error for the window — its chart has no Trades
            // layer — so a refusal is noted once per window at debug and never retried.
            let trades = match feed.subscribe_trades(inst) {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::debug!(
                        "ensure_depth({venue}, {inst}): no trade stream for the tick chart: {e}"
                    );
                    None
                }
            };
            trade_depth.insert(key, TradeDepthSubs { depth, trades });
        }
        Err(e) => retries.note_error(
            &depth_key,
            &e,
            &format!("ensure_depth({venue}, {inst}): subscribe_depth"),
        ),
    }
}

/// Start (once) a live Polymarket L2 book + trade stream for `token` (the YES-outcome token-id)
/// so a cockpit window renders that token's real book from the shared `BookStore` under venue
/// `"polymarket"`. The polymarket feed's `subscribe_book` emits `LiveDataSink::book`, which
/// [`GuiFeedSink`](crate::data::data_sink::GuiFeedSink)'s `book` lands in the `BookStore`.
/// The cockpit's analog of [`ensure_depth`]; dedups via `poly_subs`, whose entry KEEPS the minted
/// ids ([`PolyBookSubs`]) so [`reap_orphaned_trade_cockpit_streams`] stops both streams once no
/// cockpit window shows the token anymore (before that existed, only the feed's `shutdown()` on
/// exit ever stopped them). A missing feed, an empty/placeholder token, or a subscribe error is
/// logged and skipped — feed failure never crashes the GUI (the cockpit just stays STALE).
///
/// The feed runs on its own thread and routes through the crate's proxy (`POLY_*` env, resolved
/// internally); it connects only when the WS proxy is enabled AND reachable (US-geo-block),
/// otherwise it retries with backoff and the book stays empty/STALE — graceful degradation.
///
/// # The trade leg no longer vanishes under the book gate
///
/// `poly_subs.insert(token)` is deliberately driven by the BOOK leg — the book is what the ladder
/// paints, so recording the token once it is live is correct and must stay (re-running
/// `subscribe_book` every frame would be the other bug). But the `subscribe_trades` leg ran inside
/// that same `Ok` arm and its failure only warned: `poly_subs` then suppressed every later call, so
/// that token's trade tape was permanently absent — the cockpit painted a book with no prints and
/// nothing could reopen it. The leg now carries its own [`RetryLane::PolyTrades`] record, so a
/// later frame re-attempts *just* the trades subscribe while the live book is left alone. The book
/// leg's own failure is unchanged in outcome (still retried, still records nothing) but is now
/// throttled by the same schedule instead of re-dialling on every frame.
pub fn ensure_poly_book(
    feeds: &mut FeedMap,
    poly_subs: &mut HashMap<String, PolyBookSubs>,
    retries: &mut FeedRetries,
    token: &str,
) {
    if token.is_empty() || token == POLY_PLACEHOLDER_TOKEN {
        return;
    }
    let book_key = RetryKey::poly_book(token);
    let trades_key = RetryKey::poly_trades(token);
    if let Some(sub) = poly_subs.get_mut(token) {
        // The book is live and must NOT be re-subscribed; only the trade leg can still be
        // outstanding — which is exactly what used to be unreachable from here.
        if retries.is_retry_due(&trades_key)
            && let Some(feed) = feeds.get_mut(POLY_VENUE)
            && let Some(id) = try_poly_trades(&mut **feed, retries, token, &trades_key)
        {
            sub.trades = Some(id);
        }
        return;
    }
    if retries.is_blocked(&book_key) {
        return;
    }
    let Some(feed) = feeds.get_mut(POLY_VENUE) else {
        if retries.note_missing(&book_key) {
            tracing::warn!(
                "ensure_poly_book({token}): no polymarket feed registered — the cockpit will stay \
                 STALE; will retry once one is registered"
            );
        }
        return;
    };
    match feed.subscribe_book(token) {
        Ok(book_id) => {
            if let Some(n) = retries.clear(&book_key) {
                tracing::info!(
                    "ensure_poly_book({token}): subscribe_book recovered after {n} failed \
                     attempt(s)"
                );
            }
            let trades = try_poly_trades(&mut **feed, retries, token, &trades_key);
            poly_subs.insert(token.to_string(), PolyBookSubs { book: book_id, trades });
        }
        Err(e) => {
            retries.note_error(&book_key, &e, &format!("ensure_poly_book({token}): subscribe_book"))
        }
    }
}

/// The cockpit's trade-tape leg, in one place so its FIRST attempt (inside the book's `Ok` arm) and
/// its RETRIES (a later frame, once `poly_subs` is already recorded) can never diverge — the
/// divergence being exactly how a retry path comes not to exist at all. Returns the minted id on
/// success so both callers record it in the entry's [`PolyBookSubs::trades`].
fn try_poly_trades(
    feed: &mut (dyn DataClient + Send),
    retries: &mut FeedRetries,
    token: &str,
    trades_key: &RetryKey,
) -> Option<SubscriptionId> {
    match feed.subscribe_trades(token) {
        Ok(id) => {
            if let Some(n) = retries.clear(trades_key) {
                tracing::info!(
                    "ensure_poly_book({token}): the trade leg recovered after {n} failed \
                     attempt(s) — prints resume"
                );
            }
            Some(id)
        }
        Err(e) => {
            retries.note_error(
                trades_key,
                &e,
                &format!("ensure_poly_book({token}): subscribe_trades"),
            );
            None
        }
    }
}
