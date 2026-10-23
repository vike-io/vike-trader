//! Start and stop one series: the slot bundles, the two `ensure_*_on` and `stop_series`.

use super::*;
use crate::backend::venue_routing::{venue_bar_instrument, venue_of_key};
use crate::ui::workspace::DEFAULT_VENUE;
use vike_chart::{DisplayTz, model};
use vike_data::SubscriptionId;
use vike_orderflow::{bar_agg, tickvol};

/// The venue-subscription bookkeeping every feed start/stop touches, borrowed as one bundle —
/// the [`CoreSyncState`](crate::ui::core_sync::CoreSyncState) pattern applied to the write side of
/// the feed layer. The five are inseparable in practice: a feed is started at most once per key
/// (`spawned`), through its venue's client (`feeds`), and the returned id must be remembered
/// (`subs`) or the stream can never be stopped again — while `unroutable` records the keys that
/// got none of that because their venue has no client at all, and `retries` the keys that got none
/// of it because the venue was reached and said no.
pub struct FeedSlots<'a> {
    /// The venue-keyed live clients (`App::feeds`).
    pub feeds: &'a mut FeedMap,
    /// `chart/feed key -> live subscription id` (`App::subs`), the handle `unsubscribe` needs.
    pub subs: &'a mut HashMap<String, SubscriptionId>,
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
    /// actually subscribed". Shared with [`ensure_depth`]/[`ensure_poly_book`], which take it as a
    /// bare parameter because they own no [`FeedSlots`]; see [`FeedRetries`] for the one retry
    /// policy all four sites follow, and [`RetryLane`] for why the reaper sweeps only part of it.
    pub retries: &'a mut FeedRetries,
}

/// The render-side slots a feed owns for as long as it runs: the per-key chart state it fills,
/// the Data-manager "deleted" set, and the two client-side aggregator maps a tick/volume or
/// orderflow series is built in. Separate from [`FeedSlots`] because `ensure_depth` /
/// `ensure_poly_book` touch none of it.
pub struct SeriesSlots<'a> {
    /// Per-chart-key render state (`App::charts`).
    pub charts: &'a mut HashMap<String, model::ChartState>,
    /// Data-manager-deleted keys: the feed may still run, the GUI stops syncing (`App::hidden`).
    pub hidden: &'a mut HashSet<String>,
    /// Tick/volume aggregators, `chart key -> (venue, symbol, agg)` (`App::aggs`).
    pub aggs: &'a mut HashMap<String, (String, String, tickvol::TickVolAgg)>,
    /// Orderflow (footprint/CVD) aggregators, same keyspace (`App::of_aggs`).
    pub of_aggs: &'a mut HashMap<String, (String, String, bar_agg::OrderflowAgg)>,
}

/// Which series [`ensure_feed_on`] should bring up: the four values that used to be four loose
/// arguments on `App::ensure_feed_on`. Bundled so the function stays under clippy's
/// `too_many_arguments` threshold once the two state bundles are also parameters.
pub struct SeriesSpec<'a> {
    /// Venue slug the subscription routes to (`"binance"`, `"okx"`, …).
    pub venue: &'a str,
    /// The window's symbol, BEFORE venue-native instrument resolution.
    pub symbol: &'a str,
    /// Kline interval (`"1m"`), or a client-side tick/volume interval (`"100t"`/`"10v"`).
    pub interval: &'a str,
    /// The catalog asset class, when the symbol came from a catalog hit — drives
    /// [`venue_bar_instrument`]'s one refusal (`Combo`). `None` = the spot/quick-pick path.
    pub asset_class: Option<vike_model::AssetClass>,
}

/// Subscribe `venue`'s raw trade tape for `symbol`, once (idempotent via `spawned`'s
/// trade-key entry). Shared by the tick/volume branch of [`ensure_feed_on`] (Task B5, now
/// venue-aware) and SP2 orderflow's `of_wanted` processing (Task 7, now venue-aware too —
/// any venue with a `subscribe_trades` feed drives orderflow, live-only) — both need the same
/// underlying feed, just different per-(venue,symbol) consumers (`aggs` vs `of_aggs`). Trade key mirrors
/// [`workspace::series_key`]'s convention so two venues' trade feeds for the same symbol never
/// collide: `"{symbol}@trades"` for [`DEFAULT_VENUE`] (byte-identical to the pre-venue-aware
/// behavior), else `"{venue}:{symbol}@trades"` — the exact convention
/// [`orphaned_trade_feed_keys`] parses back out.
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
/// [`FeedSlots::retries`], which reopens the attempt on that leg's own backoff; see [`FeedRetries`]
/// for the cadence and for the one error kind that is deliberately never retried.
pub fn ensure_trade_feed_on(f: &mut FeedSlots<'_>, venue: &str, symbol: &str) {
    let trade_key = if venue == DEFAULT_VENUE {
        format!("{symbol}@trades")
    } else {
        format!("{venue}:{symbol}@trades")
    };
    // Same three-set shape as `ensure_feed_on`'s kline arm: `spawned` is recorded unconditionally
    // (its population is unchanged by either fix), while `unroutable` (venue absent) and `retries`
    // (venue said no) mark the ones that never subscribed and are what make the attempt repeatable.
    let retry_key = RetryKey::series(&trade_key);
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
                    f.subs.insert(trade_key.clone(), id);
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

/// Spawn a feed for `(venue, symbol, interval)` if not already running, routing the bar
/// subscription to `venue`'s [`vike_data::DataClient`] (cross-exchange symbol search). Kline
/// intervals subscribe that venue's live klines; the `charts`/`subs`/`spawned` slots are keyed
/// by [`workspace::series_key`] so Binance stays byte-identical (`"SYMBOL@interval"`) while
/// other venues are namespaced (`"venue:SYMBOL@interval"`) and never collide on a shared symbol.
/// Tick/volume intervals (Task B5) have no venue kline feed of their own — they're built
/// client-side from `venue`'s raw trade tape (venue-aware: any venue whose feed implements
/// `subscribe_trades` can drive tick/vol charts), so those spawn `venue`'s own trade feed
/// ([`ensure_trade_feed_on`]) plus a per-chart-key aggregator in `aggs`.
///
/// `shutting_down` is `App::shutdown`'s already-loaded value (see the module doc): once the
/// window-close is observed, no NEW live feed may start — a fresh subscription would open another
/// blocking socket read the bounded teardown would then have to wait on.
///
/// # A missing venue no longer permanently suppresses the subscribe
///
/// This used to run `spawned.insert(key)` **before** `feeds.get_mut(venue)`, and `spawned` was the
/// *only* gate — so ensuring a venue with no registered client suppressed the subscribe forever:
/// no retry ever, registering the feed afterwards did not help, and a single `warn!` was the only
/// trace. The reaper could not recover it either — [`orphaned_feed_keys`] only frees a slot whose
/// window is *gone*, and this window is very much alive, so the key never became orphaned. The
/// chart simply painted nothing, forever.
///
/// **This was reachable from the UI, not hypothetical.** The shell (`vike-app` then) registered
/// exactly six live clients (binance/bybit/okx/aster/hyperliquid/polymarket) while its Symbol
/// picker searched a TWELVE-venue [`vike_catalog`] — and since the `fat` build went (2026-09-09)
/// the desktop registers NO live client at all, so every venue reaches it. When this was written,
/// five of the six extra venues reached this function with an asset
/// class [`venue_bar_instrument`] passes straight through — alpaca (`Equity`/`CryptoSpot`) and
/// oanda/dukascopy/fxcm/ctrader (`Fx`/`Cfd`) — so picking one of their instruments lands here with
/// a venue that has no feed. (Deribit, the one catalog present in *every* build, was NOT among them
/// for a long time, and for the WRONG reason: its rows were all `Option`/`CryptoFuture`/
/// `CryptoPerp`, which `venue_bar_instrument` rejected for every venue outside a five-venue
/// allowlist, so those returned BEFORE the slot — before `spawned`, which is also
/// `sync_from_core`'s fold filter — and a deribit option chart showed `NO FEED` while the backend's
/// store held its bars. That allowlist is deleted: a derivative of any venue now reaches the slot
/// like any other class, and a venue the datahub cannot serve answers `Unsupported` there.
/// `restore_workspace` replays whatever venue a persisted `workspace.json` holds on any build.)
///
/// **The obvious fix — insert into `spawned` only after the lookup succeeds — is wrong here, and
/// the reason is worth stating.** `spawned` is not only this function's idempotency gate: it is
/// also the filter [`sync_from_core`](crate::ui::core_sync) folds core snapshots through
/// (`if !spawned.contains(&key) { continue }`). In `--observe` mode — every launch since the `fat`
/// build went — the shell builds an **empty** `feeds` map — no local core, no venue clients — and
/// renders charts purely from the remote
/// daemon's streamed bars. Making `spawned` conditional on a feed lookup would leave it empty
/// there, and every observed chart would silently stop rendering. So `spawned` keeps its exact
/// former population ("the GUI wants this series"), and the *subset that never actually
/// subscribed* is tracked separately in [`FeedSlots::unroutable`]; the subscribe is attempted when
/// the key is new **or** is currently unroutable. Two deliberate choices around that, both about
/// *not* trading a silent failure for a noisy one:
///
/// - **The miss is warned once, not once per frame.** Every caller here is per-frame (the window
///   loop's compare + foreign-source-study fan-out, and [`retry_unroutable`]'s own pass), and a
///   venue the connected datahub does not advertise stays unregistered for as long as that server
///   is the one connected — a bare `warn!` on the retry path would be a ~60 Hz log loop that can
///   never succeed, which is the failure mode behind this workspace's 341 GB trace-file incident.
///   `unroutable` is the bounded record; see [`FeedSlots::unroutable`]. This mirrors #937, whose
///   whole point was that the recovered lossy path warns per *event*, "not per frame".
/// - **A failed `subscribe_bars` is retryable too — but on a backoff, not per frame.** #946 left
///   this one open on purpose ("retrying *that* would resubscribe against a live venue socket every
///   frame, which needs backoff to be safe — real machinery, deliberately not smuggled in here"),
///   and named it the remaining gap. The machinery now exists: [`FeedRetries`]. The distinction it
///   preserves is the one #946 drew — the venue was reached and *answered*, so the attempt really
///   happened and the re-attempt costs a venue call — which is why the cadence differs from the
///   missing-venue case rather than the outcome differing. A blip recovers in a second; a venue
///   that keeps rejecting the symbol settles at one re-ask a minute, with its record visible in
///   `retries` the whole time instead of a single warn scrolling away. The one exception is a
///   DECLARED-capability refusal (`LiveDataError::Unsupported`), which is provably permanent and is
///   recorded-but-never-retried; see [`FeedRetries`].
///
/// Consistency note with #937, which fixed this same insert-only shape in the aggTrades backfill
/// lane: that fix deliberately left the run-once spawn gate closed and recovered the data
/// elsewhere, because there the guarded work (a 300-page REST backfill) really had run. The shared rule is *the gate
/// must reflect what actually happened* — and it points opposite ways in the two lanes precisely
/// because in this one, nothing happened. #937's other rule is honoured literally: its recovered
/// lossy path warns per *event*, "not per frame", which is why the miss here is warned once per key
/// rather than on every one of the ~60 ensure calls a second each window makes.
pub fn ensure_feed_on(
    f: &mut FeedSlots<'_>,
    s: &mut SeriesSlots<'_>,
    spec: SeriesSpec<'_>,
    display_tz: DisplayTz,
    shutting_down: bool,
) {
    let SeriesSpec { venue, symbol, interval, asset_class } = spec;
    // Shutting down (window-close requested / `on_exit` running): never start a NEW live feed —
    // a fresh subscription would open another blocking socket read the bounded teardown would
    // then have to wait on. Inert on the running path (a single Relaxed load; see `on_exit`).
    if shutting_down {
        return;
    }
    // Resolve the venue-native bar instrument for this product. Only a `Combo` is skipped
    // (`None`, BEFORE the `spawned` insert below, so such a series is never wanted); every other
    // class — every derivative on EVERY venue included — yields `inst == symbol`, so the key
    // below is unchanged for every case that ever worked. What a venue can serve is the
    // datahub's answer (`subscribe_bars` below), not a table here.
    let Some(inst) = venue_bar_instrument(venue, symbol, asset_class) else {
        return;
    };
    let key = workspace::series_key(venue, &inst, interval);
    s.hidden.remove(&key); // re-adding a deleted series just unhides it
    match tickvol::BarKind::parse(interval) {
        tickvol::BarKind::Kline(_) => subscribe_kline(f, &key, venue, &inst, interval),
        kind => {
            // Tick/Volume: the underlying feed is the raw trade tape, spawned ONCE per
            // (venue, symbol) (a second tick/volume interval on the same symbol reuses it) —
            // the aggregator, not the feed, is per chart key. Venue-aware (any venue whose
            // feed implements `subscribe_trades` can drive tick/vol charts, not just Binance).
            ensure_trade_feed_on(f, venue, &inst);
            // `kind` is guaranteed non-Kline on this match arm (the Kline arm above is the
            // only other variant), so `TickVolAgg::new` never returns `None` here.
            s.aggs.entry(key.clone()).or_insert_with(|| {
                (venue.to_string(), inst.clone(), tickvol::TickVolAgg::new(&kind).unwrap())
            });
        }
    }
    s.charts.entry(key.clone()).or_default().set_tz(display_tz);
}

/// The kline arm of [`ensure_feed_on`] — one series key's `subscribe_bars` through its venue's
/// client — in a function of its own so [`retry_unroutable`] can re-run EXACTLY this when a venue's
/// feed turns up after the key was first ensured.
///
/// `spawned` is recorded FIRST and unconditionally, exactly as before — it doubles as
/// `sync_from_core`'s fold filter, and `--observe` mode registers NO feeds at all, so gating it on
/// the venue lookup would stop every remote-snapshot chart from rendering (see
/// [`ensure_feed_on`]'s doc). `unroutable` (venue absent) and `retries` (venue said no) are the two
/// subsets that never actually subscribed, and they are what reopen the attempt on a later frame —
/// the first per frame because re-checking is free, the second only once its own backoff has
/// elapsed.
fn subscribe_kline(f: &mut FeedSlots<'_>, key: &str, venue: &str, inst: &str, interval: &str) {
    let retry_key = RetryKey::series(key);
    let first_ensure = f.spawned.insert(key.to_string());
    if first_ensure || f.unroutable.contains(key) || f.retries.is_retry_due(&retry_key) {
        match f.feeds.get_mut(venue) {
            Some(feed) => {
                f.unroutable.remove(key);
                match feed.subscribe_bars(inst, interval) {
                    Ok(id) => {
                        if let Some(n) = f.retries.clear(&retry_key) {
                            tracing::info!(
                                "ensure_feed({key}): subscribe_bars recovered after {n} \
                                 failed attempt(s)"
                            );
                        }
                        f.subs.insert(key.to_string(), id);
                    }
                    // A FAILED subscribe stays retryable, but on ITS OWN cadence: the venue
                    // was reached and answered, so the re-attempt costs a venue call, and
                    // every caller here is per-frame — hence the backoff rather than the
                    // free per-frame re-check the missing-venue arm below gets.
                    Err(e) => f.retries.note_error(
                        &retry_key,
                        &e,
                        &format!("ensure_feed({key}): subscribe_bars"),
                    ),
                }
            }
            None => {
                if f.unroutable.insert(key.to_string()) {
                    // ⚠ States the FACT and not a consequence. This used to end "this chart
                    // will stay empty until one is", which is false in the configuration
                    // that produces it: the chart itself fills perfectly from
                    // `WireSnapshot::bars` over the hub protocol (proto v2's "bounded bar
                    // tails for the observer chart") or from a read of the backend's store.
                    // MEASURED 2026-08-31: a thin client logged this line on repeat while
                    // drawing five hours of live BTCUSDT candles with volume, RSI and MACD.
                    // A warning that contradicts what the operator can see teaches them to
                    // ignore warnings. A venue lands here when `MdSession::sync_mount` has
                    // mounted no `DatahubFeed` for its slug — the connected datahub does not
                    // advertise it, or has not been heard from yet (`retry_unroutable` picks
                    // the key up when the feed appears); no venue socket is involved either way.
                    tracing::warn!(
                        "ensure_feed({key}): no market-data feed mounted for venue \
                         {venue:?} — live bars for this chart can only arrive from the \
                         backend (`--observe`); nothing in this process will produce \
                         them. Will retry"
                    );
                }
            }
        }
    }
}

/// **Re-attempt every wanted series whose venue had NO feed when it was ensured and has one now.**
/// The shell calls it once per frame, right after `MdSession::sync_mount` (the module doc of
/// `crates/vike-app-core/src/data/md_session.rs` is why the feeds arrive late).
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
        .filter(|k| f.spawned.contains(k.as_str()) && f.feeds.contains_key(venue_of_key(k)))
        .cloned()
        .collect();
    for key in ready {
        if let Some((venue, symbol)) = trade_key_pair(&key, DEFAULT_VENUE) {
            ensure_trade_feed_on(f, &venue, &symbol);
        } else if let Some((inst, interval)) = kline_key_parts(&key) {
            subscribe_kline(f, &key, venue_of_key(&key), inst, interval);
        }
    }
}

/// The `(instrument, interval)` a kline series key was built from: [`workspace::series_key`] read
/// back past its `venue:` prefix ([`venue_of_key`] reads the prefix itself, with the same
/// first-`:` rule). `None` for a string with no `@`, which `series_key` cannot have built.
fn kline_key_parts(key: &str) -> Option<(&str, &str)> {
    key.split_once(':').map_or(key, |(_, rest)| rest).rsplit_once('@')
}

/// **Stop ONE series, completely** — the whole per-key teardown, in the one place both callers
/// reach it.
///
/// Two sites tear a series down and they must not disagree: [`reap_orphaned_feeds`] applies this to
/// every key no window references anymore, and the Data-manager "Delete" button
/// (`crates/vike-desktop/src/app_ui.rs`'s `draw_windows`) applies it to the one row an operator picked.
/// Four steps, each of which some later frame depends on: drop the render series (`charts`), mark
/// the key `hidden` (belt and braces against a same-frame resurrection), stop the live venue
/// subscription (`subs` + `unsubscribe`), and free the `spawned` slot so a later [`ensure_feed_on`]
/// genuinely restarts the feed instead of finding it already "spawned".
///
/// # Why this is a function and not a rule
///
/// Because it was a rule, and the rule was broken. [`reap_orphaned_feeds`]'s own doc said its
/// teardown mirrored the Data-manager path "exactly", while that path carried a HAND COPY that had
/// drifted on the one line that matters here: it looked the feed up as `feeds["binance"]` outright,
/// under a comment claiming both key kinds were binance-only. That claim stopped being true the
/// moment [`workspace::series_key`] began namespacing every non-[`DEFAULT_VENUE`] series as
/// `"venue:SYMBOL@interval"`, so EVERY venue but the default one was exposed. Deleting an
/// `"okx:BTC-USDT@1m"` row removed the id from `subs`, missed the binance lookup, dropped the id on
/// the floor, and freed `spawned` anyway — the stream ran until process exit, and the next
/// [`ensure_feed_on`] opened a SECOND one on top of it. Nothing could recover that: the key had
/// already left `subs`, and a Data-manager Delete leaves the window in place (`open = false`), so
/// [`reap_orphaned_feeds`] still counts the key as LIVE and never looks at it.
///
/// So the routing is stated once, here: the unsubscribe goes to the SAME venue feed the key was
/// subscribed on, recovered from the key's own prefix by [`venue_of_key`] — the inverse of the
/// builder that put it there. A venue with no registered client (an `--observe` session has none at
/// all) is tolerated exactly as every `ensure_*` tolerates it: the slots are freed and nothing is
/// dialled.
///
/// # What it deliberately does NOT touch
///
/// Any aggregator. The two callers need OPPOSITE things there, so folding an `aggs`/`of_aggs`
/// removal in would put two contradictory preconditions under one name: [`reap_orphaned_feeds`]
/// drops dead aggregators in BULK afterwards (step 2a) precisely because it must then recompute
/// which SHARED trade tapes are still needed, while the Data-manager caller removes its own key's
/// entry by hand because that key IS still live — its window is still in `wins`, so step 2a would
/// keep the aggregator. See that function's doc for the shared-tape argument.
pub fn stop_series(f: &mut FeedSlots<'_>, s: &mut SeriesSlots<'_>, key: &str) {
    s.charts.remove(key);
    s.hidden.insert(key.to_string());
    if let Some(id) = f.subs.remove(key) {
        // Route the unsubscribe to the SAME venue feed the key was subscribed on — a non-Binance
        // key is namespaced `"venue:SYMBOL@interval"` (see `workspace::series_key`), so its venue
        // is recoverable from the key prefix.
        if let Some(feed) = f.feeds.get_mut(venue_of_key(key)) {
            feed.unsubscribe(id);
        }
    }
    f.spawned.remove(key);
}
