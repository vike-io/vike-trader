//! Start and stop one series: the slot bundles, the two `ensure_*_on` and `stop_series`.

use super::*;
use crate::data::chart_key::{venue_bar_instrument, venue_of_key};
use crate::ui::startup::SeriesSpec;
use vike_chart::chart::state;
use vike_data::DataClientSubscriptionId;
use vike_orderflow::{bar_agg, tickvol};
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;

/// The venue-subscription bookkeeping every feed start/stop touches, borrowed as one bundle —
/// the [`CoreSyncState`](crate::ui::core_sync::CoreSyncState) pattern applied to the write side of
/// the feed layer. The five are inseparable in practice: a feed is started at most once per key
/// (`spawned`), through its venue's client (`feeds`), and the returned id must be remembered
/// (`subscriptions`) or the stream can never be stopped again — while `unroutable` records the keys that
/// got none of that because their venue has no client at all, and `retries` the keys that got none
/// of it because the venue was reached and said no.
pub struct FeedSlots<'a> {
    /// The venue-keyed live clients (`App::feeds`).
    pub feeds: &'a mut DataClientMap,
    /// `chart/feed key -> live subscription id` (`App::subscriptions`), the handle `unsubscribe` needs.
    pub subscriptions: &'a mut HashMap<String, DataClientSubscriptionId>,
    /// Every key the GUI has ensured (`App::spawned`) — "this series is wanted".
    ///
    /// Deliberately NOT narrowed to "successfully subscribed": this set is also
    /// [`sync_from_core`](crate::ui::core_sync)'s fold filter, and `--observe` mode has no feeds at all
    /// yet must still render remote bars. See [`ensure_feed_on`] for the full argument.
    pub spawned: &'a mut HashSet<String>,
    /// The subset of `spawned` whose venue had **no registered feed** at the last attempt
    /// (`App::unroutable`) — i.e. wanted, but never actually subscribed.
    ///
    /// Two jobs: it reopens the subscribe (a key in here is retried on the next `ensure_*`, or by
    /// [`retry_unroutable`], instead of being suppressed by `spawned`), and it is the warn-once gate
    /// that keeps a missing venue from logging on every frame. Membership is *current* state, not a
    /// warned-once-ever tombstone — cleared the moment the venue's client is found, and swept by
    /// [`reap_orphaned_feeds`] when nothing wants the key anymore, so it can never become the
    /// insert-only set this whole fix exists to remove. Empty on the normal path; a non-empty entry
    /// means that chart is painting nothing — or is waiting for the datahub's handshake to name its
    /// venue (the feeds are mounted from it, so every key ensured before it lands is in here).
    pub unroutable: &'a mut HashSet<String>,
    /// Every subscribe leg the venue **answered no** to and that has not since succeeded
    /// (`App::feed_retries`) — the sibling of `unroutable` for the other half of "wanted, but not
    /// actually subscribed". Shared with [`ensure_depth`]/[`ensure_polymarket_book`], which take it as a
    /// bare parameter because they own no [`FeedSlots`]; see [`DatahubFeedRetries`] for the one retry
    /// policy all four sites follow, and [`DatahubFeedRetryLane`] for why the reaper sweeps only part of it.
    pub retries: &'a mut DatahubFeedRetries,
}

/// The render-side slots a feed owns for as long as it runs: the per-key chart state it fills,
/// the Data-manager "deleted" set, and the two client-side aggregator maps a tick/volume or
/// orderflow series is built in. Separate from [`FeedSlots`] because `ensure_depth` /
/// `ensure_polymarket_book` touch none of it.
pub struct SeriesSlots<'a> {
    /// Per-chart-key render state (`App::charts`).
    pub charts: &'a mut HashMap<String, state::ChartState>,
    /// Data-manager-deleted keys: the feed may still run, the GUI stops syncing (`App::hidden`).
    pub hidden: &'a mut HashSet<String>,
    /// Tick/volume aggregators, `chart key -> (venue, symbol, agg)` (`App::tick_volume_aggregators`).
    pub tick_volume_aggregators: &'a mut HashMap<String, (String, String, tickvol::TickVolAgg)>,
    /// Orderflow (footprint/CVD) aggregators, same keyspace (`App::orderflow_aggregators`).
    pub orderflow_aggregators: &'a mut HashMap<String, (String, String, bar_agg::OrderflowAgg)>,
}

/// Subscribe `venue`'s raw trade tape for `symbol`, once (idempotent via `spawned`'s
/// trade-key entry). Shared by the tick/volume branch of [`ensure_feed_on`] (Task B5, now
/// venue-aware) and SP2 orderflow's `orderflow_wanted` processing (Task 7, now venue-aware too —
/// any venue with a `subscribe_trades` feed drives orderflow, live-only) — both need the same
/// underlying feed, just different per-(venue,symbol) consumers (`tick_volume_aggregators` vs `orderflow_aggregators`). The trade key
/// IS a [`chart_key::series_key`], `"{venue}:{symbol}@trades"` for every venue, so two venues'
/// trade feeds for the same symbol never collide and `orphaned_trade_feed_keys` parses it back
/// with the same one split.
///
/// **Venue lookup first, `spawned` second** — the same ordering fix [`ensure_feed_on`] documents,
/// applied here because this function had the identical defect in a *worse* form: its missing-feed
/// path had no `else` arm at all, so a tick/volume or orderflow chart on a venue with no registered
/// client burned its trade-tape slot in complete silence. The miss is now recorded in `unroutable`
/// (warn once, retry freely) rather than logged per frame. Orderflow's caller is per-frame, so it
/// re-asks by itself; a tick/volume chart's is NOT (its primary series is ensured once, when the
/// window opens), and [`retry_unroutable`] is what re-asks for it.
///
/// **A FAILED `subscribe_trades` no longer burns the slot either** — the twin of the same change in
/// [`ensure_feed_on`], and the gap #946 named. The tape a tick/volume chart and every orderflow
/// overlay on this `(venue, symbol)` share is the *only* source those aggregators have, so one
/// transient error used to leave every one of them permanently empty. It is now recorded in
/// [`FeedSlots::retries`], which reopens the attempt on that leg's own backoff; see [`DatahubFeedRetries`]
/// for the cadence and for the one error kind that is deliberately never retried.
pub fn ensure_trade_feed_on(f: &mut FeedSlots<'_>, venue: &str, symbol: &str) {
    let trade_key = chart_key::series_key(venue, symbol, chart_key::TRADE_TAPE_INTERVAL);
    // `spawned` is recorded unconditionally, as `ensure_feed_on` records it
    // (its population is unchanged by either fix), while `unroutable` (venue absent) and `retries`
    // (venue said no) mark the ones that never subscribed and are what make the attempt repeatable.
    let retry_key = DatahubFeedRetryKey::series(&trade_key);
    let first_ensure = f.spawned.insert(trade_key.clone());
    if !first_ensure && !f.unroutable.contains(&trade_key) && !f.retries.is_retry_due(&retry_key) {
        return;
    }
    match f.feeds.get_mut(venue) {
        Some(feed) => {
            f.unroutable.remove(&trade_key);
            match feed.subscribe_trades(symbol) {
                Ok(id) => {
                    if let Some(n) = f.retries.clear(&retry_key) {
                        tracing::info!(
                            "ensure_trade_feed_on({trade_key}): subscribe_trades recovered after \
                             {n} failed attempt(s)"
                        );
                    }
                    f.subscriptions.insert(trade_key.clone(), id);
                }
                Err(e) => f.retries.note_error(
                    &retry_key,
                    &e,
                    &format!("ensure_trade_feed_on({trade_key}): subscribe_trades"),
                ),
            }
        }
        None => {
            if f.unroutable.insert(trade_key.clone()) {
                tracing::warn!(
                    "ensure_trade_feed_on({trade_key}): no feed registered for venue {venue:?} — \
                     no trades will arrive for it; will retry once one is registered"
                );
            }
        }
    }
}

/// Want the series `(venue, symbol, interval)`: create its chart state, record it in `spawned`, and
/// open the one venue subscription it needs, if any. The `charts`/`subscriptions`/`spawned` slots are keyed by
/// [`chart_key::series_key`], `"venue:SYMBOL@interval"` for every venue, so two venues never
/// collide on a shared symbol.
///
/// # A kline series asks no feed
///
/// Its bars are the BACKEND's: the live snapshot's tail where the node publishes the series, else a
/// read of the backend's store (`crate::backend::split_plane`'s `series_render_source`, which gives
/// every kline series exactly one source). The one transport this desktop has, the datahub's
/// market-data wire, serves no bar lane by design (`crate::data::market_data_session::data_client`'s module doc: its
/// `subscribe_bars` is a flat `Unsupported`), so a `subscribe_bars` here could only ever be refused.
/// It used to be asked anyway, and every chart then logged two false WARNs (the CI box, 2026-11-16): "no
/// market-data feed mounted … nothing in this process will produce them" before the datahub's
/// handshake, and "subscribe_bars refused … this series stays empty" after it, while the chart drew
/// its bars from the snapshot and the store. So a kline series is only WANTED: `spawned` is
/// [`sync_from_core`](crate::ui::core_sync)'s fold filter, and that is all it needs.
///
/// # A tick/volume series rides the trade tape
///
/// Tick/volume intervals (Task B5) are built client-side from `venue`'s raw trade tape: the tape is
/// subscribed once per `(venue, symbol)` ([`ensure_trade_feed_on`], whose doc carries the
/// missing-venue and failed-subscribe rules) and an aggregator is kept per chart key in `tick_volume_aggregators`.
///
/// `spawned` keeps its population ("the GUI wants this series") whether or not anything was
/// subscribed, because it doubles as the fold filter: `--observe` mode renders every chart from the
/// remote daemon, and narrowing it to "successfully subscribed" would blank every observed chart.
///
/// `shutting_down` is `App::shutdown`'s already-loaded value (see the module doc): once the
/// window-close is observed, no NEW live feed may start — a fresh subscription would open another
/// blocking socket read the bounded teardown would then have to wait on.
pub fn ensure_feed_on(
    f: &mut FeedSlots<'_>,
    s: &mut SeriesSlots<'_>,
    spec: &SeriesSpec,
    display_time_zone: ThemeDisplayTimeZone,
    shutting_down: bool,
) {
    let (venue, symbol, interval, asset_class) =
        (spec.venue.as_str(), spec.symbol.as_str(), spec.interval.as_str(), spec.asset_class);
    // Shutting down (window-close requested / `on_exit` running): never start a NEW live feed —
    // a fresh subscription would open another blocking socket read the bounded teardown would
    // then have to wait on. Inert on the running path (a single Relaxed load; see `on_exit`).
    if shutting_down {
        return;
    }
    // A chart that names no venue or no symbol yet (an EMPTY chart: nothing is compiled in to open
    // it on, the owner's ruling of 2026-11-13) has no series to ask for, and an ask would only park
    // an unroutable key the per-frame retry re-reads forever.
    if venue.is_empty() || symbol.is_empty() {
        return;
    }
    // Resolve the venue-native bar instrument for this product. Only a `Combo` is skipped
    // (`None`, BEFORE the `spawned` insert below, so such a series is never wanted); every other
    // class — every derivative on EVERY venue included — yields `instrument == symbol`, so the key
    // below is unchanged for every case that ever worked. What a venue can serve is the
    // backend's answer (what it publishes and stores), not a table here.
    let Some(instrument) = venue_bar_instrument(venue, symbol, asset_class) else {
        return;
    };
    let key = chart_key::series_key(venue, &instrument, interval);
    s.hidden.remove(&key); // re-adding a deleted series just unhides it
    match tickvol::BarKind::parse(interval) {
        // A kline series opens NO venue subscription (this function's doc): it is only WANTED, which
        // is what lets `sync_from_core` fold the backend's bars into it.
        tickvol::BarKind::Kline(_) => {
            f.spawned.insert(key.clone());
        }
        kind => {
            // Tick/Volume: the underlying feed is the raw trade tape, spawned ONCE per
            // (venue, symbol) (a second tick/volume interval on the same symbol reuses it) —
            // the aggregator, not the feed, is per chart key. Venue-aware (any venue whose
            // feed implements `subscribe_trades` can drive tick/vol charts, not just Binance).
            ensure_trade_feed_on(f, venue, &instrument);
            // `kind` is guaranteed non-Kline on this match arm (the Kline arm above is the
            // only other variant), so `TickVolAgg::new` never returns `None` here.
            s.tick_volume_aggregators.entry(key.clone()).or_insert_with(|| {
                (venue.to_string(), instrument.clone(), tickvol::TickVolAgg::new(&kind).unwrap())
            });
        }
    }
    s.charts.entry(key.clone()).or_default().set_time_zone(display_time_zone);
}

/// **Re-attempt every wanted series whose venue had NO feed when it was ensured and has one now.**
/// The shell calls it once per frame, right after `MarketDataSession::sync_mount` (the module doc of
/// `crates/vike-app-core/src/data/market_data_session/mod.rs` is why the feeds arrive late).
///
/// # Why a pass of its own
///
/// [`FeedSlots::unroutable`] documents itself as what "reopens the subscribe", but it only does so
/// when something calls an `ensure_*` for the key AGAIN — and a chart's primary series is ensured
/// once, when the window opens, a symbol or interval is picked, or a saved workspace is restored,
/// not every frame (only the compare/study fan-out, the Trade window's depth stream, the cockpit's
/// book and the orderflow tape are re-asked per frame). While the venue list was a constant, every
/// feed existed before any window did, so that was enough. With the feeds mounted from the datahub's
/// handshake, a tick or volume chart restored at launch asks BEFORE the feed exists and would sit
/// on its missed `subscribe_trades` for the life of the process.
///
/// # What it touches, and what it must not
///
/// Only keys that are `unroutable` AND still in `spawned` AND whose venue is routable now. The
/// `spawned` test is the one that matters: a Data-manager Delete frees the slot while the window
/// stays (so the `unroutable` sweep keeps the key), and re-subscribing it here would resurrect a
/// series the operator just deleted. The healthy path holds nothing in `unroutable`, so this is a
/// pass over an empty set; a venue the server does not advertise leaves its keys where they are, at
/// one hash lookup each and no log line (the warn is gated on the insert).
pub fn retry_unroutable(f: &mut FeedSlots<'_>) {
    let ready: Vec<String> = f
        .unroutable
        .iter()
        .filter(|k| {
            f.spawned.contains(k.as_str())
                && venue_of_key(k).is_some_and(|v| f.feeds.contains_key(v))
        })
        .cloned()
        .collect();
    // Only a trade tape can be here: a kline series asks no feed (`ensure_feed_on`'s doc).
    for key in ready {
        if let Some((venue, symbol)) = trade_key_pair(&key) {
            ensure_trade_feed_on(f, &venue, &symbol);
        }
    }
}

/// **Stop ONE series, completely** — the whole per-key teardown, in the one place both callers
/// reach it.
///
/// Two sites tear a series down and they must not disagree: [`reap_orphaned_feeds`] applies this to
/// every key no window references anymore, and the Data-manager "Delete" button
/// (`crates/vike-desktop/src/app_ui/mod.rs`'s `draw_windows`) applies it to the one row an operator picked.
/// Four steps, each of which some later frame depends on: drop the render series (`charts`), mark
/// the key `hidden` (belt and braces against a same-frame resurrection), stop the live venue
/// subscription (`subscriptions` + `unsubscribe`), and free the `spawned` slot so a later [`ensure_feed_on`]
/// genuinely restarts the feed instead of finding it already "spawned".
///
/// # Why this is a function and not a rule
///
/// Because it was a rule, and the rule was broken. [`reap_orphaned_feeds`]'s own doc said its
/// teardown mirrored the Data-manager path "exactly", while that path carried a HAND COPY that had
/// drifted on the one line that matters here: it looked the feed up as `feeds["binance"]` outright,
/// under a comment claiming both key kinds were binance-only. That claim stopped being true the
/// moment [`chart_key::series_key`] began namespacing every non-Binance series as
/// `"venue:SYMBOL@interval"`, so EVERY venue but Binance was exposed. Deleting an
/// `"okx:BTC-USDT@1m"` row removed the id from `subscriptions`, missed the binance lookup, dropped the id on
/// the floor, and freed `spawned` anyway — the stream ran until process exit, and the next
/// [`ensure_feed_on`] opened a SECOND one on top of it. Nothing could recover that: the key had
/// already left `subscriptions`, and a Data-manager Delete leaves the window in place (`open = false`), so
/// [`reap_orphaned_feeds`] still counts the key as LIVE and never looks at it.
///
/// So the routing is stated once, here: the unsubscribe goes to the SAME venue feed the key was
/// subscribed on, recovered from the key's own prefix by `venue_of_key` — the inverse of the
/// builder that put it there. A venue with no registered client (an `--observe` session has none at
/// all) is tolerated exactly as every `ensure_*` tolerates it: the slots are freed and nothing is
/// dialled. A subscribed key that names NO venue cannot be routed at all; no builder makes one, so
/// it is logged as an error (its stream would leak) rather than sent to a guessed venue.
///
/// # What it deliberately does NOT touch
///
/// Any aggregator. The two callers need OPPOSITE things there, so folding an `tick_volume_aggregators`/`orderflow_aggregators`
/// removal in would put two contradictory preconditions under one name: [`reap_orphaned_feeds`]
/// drops dead aggregators in BULK afterwards (step 2a) precisely because it must then recompute
/// which SHARED trade tapes are still needed, while the Data-manager caller removes its own key's
/// entry by hand because that key IS still live — its window is still in `wins`, so step 2a would
/// keep the aggregator. See that function's doc for the shared-tape argument.
pub fn stop_series(f: &mut FeedSlots<'_>, s: &mut SeriesSlots<'_>, key: &str) {
    s.charts.remove(key);
    s.hidden.insert(key.to_string());
    if let Some(id) = f.subscriptions.remove(key) {
        // Route the unsubscribe to the SAME venue feed the key was subscribed on: every key is
        // `"venue:SYMBOL@interval"` (see `chart_key::series_key`), so its venue is its prefix.
        match venue_of_key(key) {
            Some(venue) => {
                if let Some(feed) = f.feeds.get_mut(venue) {
                    feed.unsubscribe(id);
                }
            }
            None => tracing::error!(
                "stop_series({key}): the key names no venue, so its subscription cannot be \
                 stopped and its stream leaks"
            ),
        }
    }
    f.spawned.remove(key);
}
