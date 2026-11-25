//! The Trade window's depth stream and the cockpit's Polymarket book + trade streams.

use super::*;
use crate::ui::windows::POLYMARKET_PLACEHOLDER_TOKEN;
use vike_data::DataClientSubscriptionId;

/// What one `trade_depth` or `polymarket_subscriptions` entry OWNS: the live subscription ids its `ensure_*`
/// minted, for the same two spenders. A `trade_depth` entry holds what [`ensure_depth`] minted for its
/// `(venue, native symbol)` key. Kept — instead of the id being dropped on the floor, which is what
/// this map's `HashSet` predecessor did — so [`reap_orphaned_trade_cockpit_streams`] can stop
/// exactly this stream when the last Trade window on the book is deleted. (A backend SWITCH keeps
/// these streams — venue truth, split-plane B2.)
///
/// ⚠ It held a second id until the Trade window's final review (M-4): the DOM's non-Binance `"1m"`
/// `subscribe_bars` leg, opened "so that venue's PAPER engine fills". The desktop has run no engine
/// since it lost its local core, the datahub feed refuses the bar lane (`Unsupported`), and the leg
/// logged one warn per instrument a Trade window opened — so it is deleted with its retry lane, not
/// kept as a second subscription that feeds nothing (Ruling R6: the Trade window opens no bar feed).
///
/// A `polymarket_subscriptions` entry is the cockpit's: [`ensure_polymarket_book`]'s pair for its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L2BookSubscriptions {
    /// The id of what the window reads its market from, the handle `unsubscribe` needs to stop it,
    /// and the leg whose success records the entry: the Trade window's `subscribe_depth`, or
    /// `subscribe_quotes` on a venue that declares no L2 lane; the cockpit's `subscribe_book` (the
    /// ladder paints).
    pub book: DataClientSubscriptionId,
    /// `subscribe_trades`' id, once it has succeeded: the print stream the Trade window's tick chart
    /// paints bubbles from. `None` for a venue that serves no trade stream, which costs the chart its
    /// Trades layer and nothing else — the book leg above never waits on it; on the cockpit, `None`
    /// while it is still failing/retrying under its [`DatahubFeedRetryLane::PolymarketTrades`] record.
    pub trades: Option<DataClientSubscriptionId>,
}

/// Start a live L2 depth stream for `(venue, instrument)` (once) so the Trade window renders that
/// venue's real book. The key is the window's venue and the venue-NATIVE symbol it trades, so
/// there is nothing to translate. Routes to the venue's feed; dedups via `trade_depth`, whose entry
/// KEEPS the minted subscription id ([`L2BookSubscriptions`]); the stream is stopped by
/// [`reap_orphaned_trade_cockpit_streams`] once no Trade window names the book anymore (before that
/// existed, only the feed's `shutdown()` on exit ever stopped it). Called every frame for the
/// window's book — idempotent. A Trade window that moves to another venue releases the old
/// venue's stream on the next reap ([`reap_orphaned_trade_cockpit_streams`] is venue-specific).
///
/// The depth stream is the ONLY one: a Trade window opens no bar feed on any venue (Ruling R6;
/// [`L2BookSubscriptions`] says what the deleted 1m leg was). A venue that declares no L2 lane (the dealer
/// venues, a broker) has its top of book asked for instead, under the same entry and the same retries;
/// the datahub only attaches such a key to a stream an operator-owned holder holds, so the window
/// shows that venue's bid and ask and opens nothing at it (decision 0193, verdict 8).
///
/// A failed depth subscribe records no `trade_depth` entry, so it stays retryable — but it used to
/// be retried on **every frame**, i.e. a ~60 Hz socket-and-log loop against a live venue. It is
/// throttled by the [`DatahubFeedRetries`] schedule, and the missing-venue arm's `warn!`, which was per
/// frame, is once per `(venue, symbol)`.
pub fn ensure_depth(
    feeds: &mut DataClientMap,
    trade_depth: &mut HashMap<(String, String), L2BookSubscriptions>,
    retries: &mut DatahubFeedRetries,
    venue: &str,
    instrument: &str,
) {
    let key = (venue.to_string(), instrument.to_string());
    // The depth stream is live and must NOT be re-subscribed.
    if trade_depth.contains_key(&key) {
        return;
    }
    let depth_key = DatahubFeedRetryKey::trade_depth(venue, instrument);
    // A previously-failed depth subscribe is retried, but only once its cooldown has elapsed —
    // this is the per-frame resubscribe loop the old code ran.
    if retries.is_blocked(&depth_key) {
        return;
    }
    let Some(feed) = feeds.get_mut(venue) else {
        if retries.note_missing(&depth_key) {
            tracing::warn!(
                "ensure_depth({venue}, {instrument}): no feed registered for venue — the Trade window's \
                 ladder will stay STALE; will retry once one is registered"
            );
        }
        return;
    };
    let lanes = vike_venues::capabilities_for(venue).live_data;
    if !lanes.depth && !lanes.book {
        match feed.subscribe_quotes(instrument) {
            Ok(depth) => {
                retries.clear(&depth_key);
                trade_depth.insert(key, L2BookSubscriptions { book: depth, trades: None });
            }
            Err(e) => retries.note_error(
                &depth_key,
                &e,
                &format!("ensure_depth({venue}, {instrument}): subscribe_quotes"),
            ),
        }
        return;
    }
    match feed.subscribe_depth(instrument) {
        Ok(depth) => {
            if let Some(n) = retries.clear(&depth_key) {
                tracing::info!(
                    "ensure_depth({venue}, {instrument}): subscribe_depth recovered after {n} failed \
                     attempt(s)"
                );
            }
            // The tick chart's print stream rides the same entry. A venue with no trade lane (or a
            // session that refuses it) is not an error for the window — its chart has no Trades
            // layer — so a refusal is noted once per window at debug and never retried.
            let trades = match feed.subscribe_trades(instrument) {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::debug!(
                        "ensure_depth({venue}, {instrument}): no trade stream for the tick chart: {e}"
                    );
                    None
                }
            };
            trade_depth.insert(key, L2BookSubscriptions { book: depth, trades });
        }
        Err(e) => retries.note_error(
            &depth_key,
            &e,
            &format!("ensure_depth({venue}, {instrument}): subscribe_depth"),
        ),
    }
}

/// Start (once) a live Polymarket L2 book + trade stream for `token` (the YES-outcome token-id)
/// so a cockpit window renders that token's real book from the shared `L2BookStore` under venue
/// `"polymarket"`. The polymarket feed's `subscribe_book` emits `DataClientSink::book`, which
/// [`DataClientSinkGuiFeed`](crate::data::data_sink::DataClientSinkGuiFeed)'s `book` lands in the `L2BookStore`.
/// The cockpit's analog of [`ensure_depth`]; dedups via `polymarket_subscriptions`, whose entry KEEPS the minted
/// ids ([`L2BookSubscriptions`]) so [`reap_orphaned_trade_cockpit_streams`] stops both streams once no
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
/// `polymarket_subscriptions.insert(token)` is deliberately driven by the BOOK leg — the book is what the ladder
/// paints, so recording the token once it is live is correct and must stay (re-running
/// `subscribe_book` every frame would be the other bug). But the `subscribe_trades` leg ran inside
/// that same `Ok` arm and its failure only warned: `polymarket_subscriptions` then suppressed every later call, so
/// that token's trade tape was permanently absent — the cockpit painted a book with no prints and
/// nothing could reopen it. The leg now carries its own [`DatahubFeedRetryLane::PolymarketTrades`] record, so a
/// later frame re-attempts *just* the trades subscribe while the live book is left alone. The book
/// leg's own failure is unchanged in outcome (still retried, still records nothing) but is now
/// throttled by the same schedule instead of re-dialling on every frame.
pub fn ensure_polymarket_book(
    feeds: &mut DataClientMap,
    polymarket_subscriptions: &mut HashMap<String, L2BookSubscriptions>,
    retries: &mut DatahubFeedRetries,
    token: &str,
) {
    if token.is_empty() || token == POLYMARKET_PLACEHOLDER_TOKEN {
        return;
    }
    let book_key = DatahubFeedRetryKey::polymarket_book(token);
    let trades_key = DatahubFeedRetryKey::polymarket_trades(token);
    if let Some(sub) = polymarket_subscriptions.get_mut(token) {
        // The book is live and must NOT be re-subscribed; only the trade leg can still be
        // outstanding — which is exactly what used to be unreachable from here.
        if retries.is_retry_due(&trades_key)
            && let Some(feed) = feeds.get_mut(POLYMARKET_VENUE)
            && let Some(id) = try_polymarket_trades(&mut **feed, retries, token, &trades_key)
        {
            sub.trades = Some(id);
        }
        return;
    }
    if retries.is_blocked(&book_key) {
        return;
    }
    let Some(feed) = feeds.get_mut(POLYMARKET_VENUE) else {
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
            let trades = try_polymarket_trades(&mut **feed, retries, token, &trades_key);
            polymarket_subscriptions
                .insert(token.to_string(), L2BookSubscriptions { book: book_id, trades });
        }
        Err(e) => {
            retries.note_error(&book_key, &e, &format!("ensure_poly_book({token}): subscribe_book"))
        }
    }
}

/// The cockpit's trade-tape leg, in one place so its FIRST attempt (inside the book's `Ok` arm) and
/// its RETRIES (a later frame, once `polymarket_subscriptions` is already recorded) can never diverge — the
/// divergence being exactly how a retry path comes not to exist at all. Returns the minted id on
/// success so both callers record it in the entry's [`L2BookSubscriptions::trades`].
fn try_polymarket_trades(
    feed: &mut (dyn DataClient + Send),
    retries: &mut DatahubFeedRetries,
    token: &str,
    trades_key: &DatahubFeedRetryKey,
) -> Option<DataClientSubscriptionId> {
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
