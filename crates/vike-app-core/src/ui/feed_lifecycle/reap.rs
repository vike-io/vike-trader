//! The per-frame reapers of series and depth/cockpit streams, with the live sets they diff.

use super::*;
use crate::backend::venue_routing::venue_of_key;
use crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN;
use crate::ui::workspace::DEFAULT_VENUE;

/// C2 tidy (FIX 3): reap any `spawned` feed no window references anymore — removing a
/// Compare symbol (chip ✕, `WinState::remove_compare`) or switching a window's primary
/// symbol away from one (FIX 1's `remove_compare` cascade) can otherwise leave that
/// symbol's feed running forever, since nothing else ever un-spawns it. [`ensure_feed_on`] is
/// idempotent per `sym@interval` (`spawned.insert`), so this is bounded by DISTINCT
/// symbols, not unbounded — but it still leaks until a matching compare is re-added.
///
/// Called once every frame (see `crates/vike-desktop/src/app_ui.rs`'s `draw_windows` — it named
/// `vike-app`'s `update` call site until 2026-09-28) rather than threaded into every
/// removal call site — cheap (a handful of windows/keys) and it catches every orphaning
/// path uniformly (a compare chip removed, a window's primary symbol switched away) with
/// no per-call-site bookkeeping to keep in sync. A mere window *close* is deliberately NOT
/// one of them: a closed window stays in `wins` (`open = false`) and keeps its feed, so its
/// key stays in the live set (see [`orphaned_feed_keys`]'s "regardless of `open`/`minimized`"
/// note). The live-set computation itself is that pure, unit-tested function — see its doc
/// for exactly what counts as "live".
///
/// The actual teardown IS the Data-manager "Delete" path: both call [`stop_series`], applied per
/// orphan and automatically here, by hand and per row there. ⚠ This paragraph used to say the two
/// "mirror … exactly" while the other side carried a hand copy that had drifted into a
/// binance-only unsubscribe — [`stop_series`]'s doc carries that post-mortem, and it is the reason
/// there is one copy now rather than a claim about two.
///
/// Feed-leak fix: two coupled cleanups follow the Kline reap. First, drop any orphaned
/// tick/volume (`aggs`) or orderflow (`of_aggs`) AGGREGATOR entry — an aggregator's key IS its
/// window's `WinState::key()`, so an entry whose key is not in [`live_window_keys`] has no
/// window backing it (its window's interval/symbol changed, or the window was deleted) and
/// would otherwise sit in the map until app restart. Second, stop any now-orphaned raw trade
/// tape (`"…@trades"`): those feeds are SHARED per `(venue, symbol)` across every aggregator on
/// that pair, so a feed is stopped ONLY once no remaining `aggs`/`of_aggs` entry needs it —
/// hence `needed` is computed AFTER the dead aggregators are dropped, and only feeds whose
/// `(venue, symbol)` fell out of `needed` are torn down (via the pure
/// [`orphaned_trade_feed_keys`]). This closes the old documented gap (a deleted tick/vol or
/// orderflow chart used to leak both its aggregator entry AND its `subscribe_trades` thread).
///
/// A third, trivial cleanup closes the loop on [`FeedSlots::unroutable`]: an unroutable key is by
/// construction NOT in `spawned`, so neither reap above can ever reach it, and the set would grow
/// insert-only for the life of the process — the exact shape [`ensure_feed_on`] was just fixed for.
/// It is swept by the same two rules, so `unroutable` always describes series the GUI still wants.
pub fn reap_orphaned_feeds(
    f: &mut FeedSlots<'_>,
    s: &mut SeriesSlots<'_>,
    wins: &[workspace::WinState],
) {
    for k in orphaned_feed_keys(wins, f.spawned) {
        stop_series(f, s, &k);
    }
    // (2a) Reap orphaned tick/vol + orderflow aggregator entries: an `aggs`/`of_aggs` key IS a
    // window's `key()`, so any entry not backed by a live window is dead. Closed/minimized
    // windows still count as live (same notion the Kline reap above uses), so this never drops
    // an aggregator for a merely-hidden chart — only for one whose window is truly gone or
    // whose key changed (interval/symbol switch).
    let live = live_window_keys(wins);
    s.aggs.retain(|k, _| live.contains(k));
    s.of_aggs.retain(|k, _| live.contains(k));
    // (2b) `needed` = the (venue, symbol) of every REMAINING aggregator (read straight off the
    // stored tuple fields). Both maps are unioned because a Tick/Volume `aggs` entry and an
    // orderflow `of_aggs` entry on the same (venue, symbol) ride the SAME trade feed. This is
    // computed after (2a) so a just-dropped aggregator no longer holds its feed open. No race
    // with the per-frame `of_wanted` registration: it runs earlier this frame and (re)inserts
    // their `of_aggs` entries before we get here, so every still-wanted pair is in `needed`.
    let mut needed: HashSet<(String, String)> = HashSet::new();
    for (venue, symbol, _) in s.aggs.values() {
        needed.insert((venue.clone(), symbol.clone()));
    }
    for (venue, symbol, _) in s.of_aggs.values() {
        needed.insert((venue.clone(), symbol.clone()));
    }
    // (2c) Stop each trade feed no remaining aggregator needs, routing the unsubscribe to the
    // right venue (recovered from the `@trades` key the same way the Kline reap recovers it).
    for key in orphaned_trade_feed_keys(&needed, f.spawned, DEFAULT_VENUE) {
        if let Some(id) = f.subs.remove(&key)
            && let Some(feed) = f.feeds.get_mut(venue_of_key(&key))
        {
            feed.unsubscribe(id);
        }
        f.spawned.remove(&key);
    }
    // (2d) Forget the `unroutable` record for any key nothing wants anymore. Without this the set
    // would be insert-only — precisely the shape this whole fix exists to remove, re-introduced one
    // layer up. An unroutable key is never in `spawned` (that is the fix), so neither reap above
    // can reach it; it is swept here by the SAME two rules they use — a kline key survives while a
    // window backs it, a `@trades` key while some remaining aggregator still needs its
    // (venue, symbol).
    let still_wanted = |k: &str| match trade_key_pair(k, DEFAULT_VENUE) {
        Some(pair) => needed.contains(&pair),
        None => live.contains(k),
    };
    f.unroutable.retain(|k| still_wanted(k.as_str()));
    // (2e) …and the same sweep for the SERIES lane of `retries`, for the same reason and under the
    // same two rules. A torn-down key falls out here even though the reaps above already freed its
    // `spawned` slot: leaving the record would carry a stale attempt count (and so an inflated
    // backoff) into the re-added series' first subscribe. The depth/cockpit lanes are deliberately
    // untouched — this function knows nothing about `trade_depth`/`poly_subs`; their teardown (and
    // the matching lane sweep) is [`reap_orphaned_trade_cockpit_streams`]'s job, run by the same
    // per-frame caller.
    f.retries.retain_series(still_wanted);
}

/// The `(venue, native symbol)` of every Trade window in `wins` — the live set
/// [`reap_orphaned_trade_cockpit_streams`] diffs `trade_depth` against. ALL windows count, regardless
/// of `open`/`minimized`, as before; only a DELETED window tears its stream down. Venue-SPECIFIC,
/// unlike the DOM's venue-blind set: a Trade window keeps its venue in its `WinState`, so a window
/// that moves to another venue releases the old venue's stream on the next reap. A window with no
/// symbol yet (the glue seeds it on the first frame) names no book, so it holds nothing alive.
pub fn live_trade_books(wins: &[workspace::WinState]) -> HashSet<(String, String)> {
    wins.iter()
        .filter(|w| w.kind == workspace::WinKind::Trade && !w.symbol.is_empty())
        .map(|w| (w.venue.clone(), w.symbol.clone()))
        .collect()
}

/// The token-ids of every Polymarket cockpit window in `wins` — [`live_trade_books`]'s cockpit
/// twin, under the same regardless-of-`open` rule. A window still carrying the placeholder token
/// (no real token was seeded) or an empty symbol contributes nothing: neither can have
/// been subscribed ([`ensure_poly_book`] refuses both), so neither can hold a stream alive.
pub fn live_poly_tokens(wins: &[workspace::WinState]) -> HashSet<String> {
    wins.iter()
        .filter(|w| w.kind == workspace::WinKind::Polymarket)
        .filter(|w| !w.symbol.is_empty() && w.symbol != POLY_PLACEHOLDER_TOKEN)
        .map(|w| w.symbol.clone())
        .collect()
}

/// [`reap_orphaned_feeds`]'s depth/cockpit sibling — the teardown path `trade_depth`/`poly_subs`
/// never had (the B1 review's flagged debt, and B2's prerequisite): stop every depth and cockpit
/// stream whose windows are GONE, at the same per-frame cadence and under the same live-set
/// notion. Before this existed, closing a DOM window left its L2 depth stream (and, non-Binance,
/// its 1m bar leg) running until process exit — harmless in observe mode (both maps empty), a real
/// socket leak in the fat build (deleted 2026-09-09; every launch is the observe mode now).
///
/// Split from [`reap_orphaned_feeds`] rather than folded into it because the two own disjoint
/// state: that reaper's whole surface is [`FeedSlots`]/[`SeriesSlots`], which deliberately carry
/// neither `trade_depth` nor `poly_subs` ([`ensure_depth`]/[`ensure_poly_book`] take them as bare
/// parameters for the same reason). The routing mirror-images the subscribe exactly:
/// [`ensure_depth`] subscribed through `feeds[venue]` and the entry's key REMEMBERS
/// that venue string, so the unsubscribe goes to `feeds[&key.0]` — the same
/// "route by the key's own venue" rule `reap_orphaned_feeds` applies via
/// [`venue_of_key`](crate::backend::venue_routing::venue_of_key); a cockpit stream always routes to
/// [`POLY_VENUE`], the one feed [`ensure_poly_book`] ever subscribes on.
///
/// A venue whose client is no longer registered (or was never registered — a `--observe` session
/// has no feeds at all) is tolerated: the entry is dropped without an unsubscribe, matching the
/// venue-missing arms of every `ensure_*`. The venue-side tolerance is the trait's:
/// [`DataClient::unsubscribe`] on an id the client no longer knows is a documented no-op, so a
/// second teardown of the same id (impossible here — the entry is removed — but reachable through
/// the backend-switch clear running after this reaper) cannot panic either.
///
/// The closing sweep drops the depth/cockpit [`FeedRetries`] records nothing can drive anymore —
/// [`reap_orphaned_feeds`]'s step (2e), applied to the lanes it deliberately leaves alone. A
/// depth/book record dies with its window (its per-frame `ensure_*` caller is gone, so it could
/// only ever sit idle); a trade record dies with the token whose retry arm re-attempts it.
/// Without this, a torn-down window's records would be the insert-only residue this module keeps
/// having to remove, and a re-opened window would inherit a stale attempt count.
pub fn reap_orphaned_trade_cockpit_streams(
    feeds: &mut FeedMap,
    trade_depth: &mut HashMap<(String, String), TradeDepthSubs>,
    poly_subs: &mut HashMap<String, PolyBookSubs>,
    retries: &mut FeedRetries,
    wins: &[workspace::WinState],
) {
    let trade_live = live_trade_books(wins);
    let poly_live = live_poly_tokens(wins);
    trade_depth.retain(|(venue, inst), sub| {
        if trade_live.contains(&(venue.clone(), inst.clone())) {
            return true;
        }
        if let Some(feed) = feeds.get_mut(venue.as_str()) {
            feed.unsubscribe(sub.depth);
            if let Some(trades) = sub.trades {
                feed.unsubscribe(trades);
            }
        }
        false
    });
    poly_subs.retain(|token, sub| {
        if poly_live.contains(token) {
            return true;
        }
        if let Some(feed) = feeds.get_mut(POLY_VENUE) {
            feed.unsubscribe(sub.book);
            if let Some(trades) = sub.trades {
                feed.unsubscribe(trades);
            }
        }
        false
    });
    // The lane sweep. TradeDepth/PolyBook records are keyed by what the live sets hold (the
    // `(venue, native symbol)` behind the id's `"venue:"` prefix / token id), so they are judged
    // against the window directly. PolyTrades records ride their token like the book does.
    retries.retain_trade_cockpit(|lane, id| match lane {
        RetryLane::TradeDepth => id
            .split_once(':')
            .is_some_and(|(v, s)| trade_live.contains(&(v.to_string(), s.to_string()))),
        RetryLane::PolyBook | RetryLane::PolyTrades => poly_live.contains(id),
        // Never consulted for Series (`retain_trade_cockpit` retains that lane unconditionally);
        // named so this match stays exhaustive when a lane is added.
        RetryLane::Series => true,
    });
}
