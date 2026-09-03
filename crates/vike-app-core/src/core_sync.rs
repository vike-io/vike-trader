//! `core_sync` — the per-frame `CoreSnapshot` → render-model fold (`sync_from_core`), moved down
//! verbatim out of `vike-app`'s CI-excluded `main.rs`.
//!
//! This is the GUI's whole read side of the R5c seam: the lossy arc-swap `CoreSnapshot` the core
//! publishes plus the live trade tape ([`TradeStore`](crate::data_sink::TradeStore)) — and, in
//! the third mode, the venue-fed direct-bar store
//! ([`DirectBarStore`](crate::data_sink::DirectBarStore), split-plane B2's kline follow-up) —
//! fold into the per-chart [`ChartState`](vike_chart::model::ChartState)s, the tick/volume
//! aggregators ([`TickVolAgg`](crate::tickvol::TickVolAgg)) and the orderflow aggregators
//! ([`OrderflowAgg`](crate::orderflow::OrderflowAgg)). It touches NO egui — every line is map
//! keying, drains and arithmetic — yet it lived in a file no gate compiles, which is exactly why
//! its history is a run of silent-data-loss bugs whose ONLY record is a code comment:
//!
//! 1. **Bar keys dropped the venue** — the fold used `format!("{symbol}@{interval}")`, so every
//!    non-Binance series landed in the Binance-shaped `charts` slot: that chart never synced, and
//!    a same-named Binance chart wrongly received its bars. Fixed by keying with
//!    [`workspace::series_key`](crate::workspace::series_key).
//! 2. **Orderflow grouped by splitting the chart key at `'@'`** — which mangled a non-Binance key
//!    `"venue:SYM@interval"` into the symbol `"venue:SYM"`, so OKX/Bybit orderflow never matched a
//!    real `(venue, symbol)` drain pair and silently received nothing. Fixed by reading the
//!    `(venue, symbol)` STORED in the `of_aggs` value.
//! 3. **The tick/volume + orderflow drain sat inside the `snap.seq` gate** — but that fold is
//!    driven by the trade tape, not the snapshot, so a workspace with ONLY tick/volume charts (no
//!    kline feed to bump `seq`) stalled until unrelated core activity happened to bump it. Fixed
//!    by draining every frame, outside the gate.
//! 4. **A drained aggTrades-backfill batch with no aggregator to land in was DISCARDED** — the
//!    fold drained `bf_rx` unconditionally and `continue`d away any batch whose
//!    `(DEFAULT_VENUE, symbol)` had no `of_aggs` entry at the frame it arrived. Closing the
//!    orderflow window, deleting it in the Data manager (`of_aggs.remove`) or letting
//!    `reap_orphaned_feeds`'s `of_aggs.retain(…)` sweep it mid-backfill therefore ate the
//!    remaining pages — **permanently**, because the spawn gate (`feed_lifecycle::
//!    should_spawn_backfill`) records each symbol in an insert-only `bf_spawned` set. (It still
//!    does. `feed_lifecycle::BackfillRetries` now reopens that gate in ONE narrow case — a walk
//!    that stopped on a REST error having delivered NOTHING — precisely because re-walking one
//!    that DID deliver would re-ingest the delivered band. So nothing about the staging argument
//!    below changes: a batch that reached `bf_rx` is still the only copy there will ever be.) The
//!    chart came back with historical CVD truncated for the rest of the process's life, silently.
//!    Fixed by [`BF_PENDING_MAX_TICKS`]-bounded per-symbol staging (`CoreSyncState::bf_pending`):
//!    a batch that cannot be applied this frame is HELD and applied when an aggregator
//!    (re)appears, and the only path that still loses ticks — the cap — logs a `warn!`.
//!
//! All four are FIXED in the code below; none had a test, because nothing compiled the file they
//! lived in. The `bug_pin_tests` module at the bottom pins today's (correct) behaviour at each of
//! the four sites, so a regression back to any of them fails the merge gate instead of silently
//! losing a venue's data again.
//!
//! **Signature note.** `sync_from_core` was `fn sync_from_core(&mut self)` on `App`. `App` is an
//! eframe type this crate cannot name, so the fields it touched become explicit parameters,
//! grouped exactly the way [`tool_views`](crate::tool_views) already groups them: read-only inputs
//! in [`CoreSyncInputs`], the mutated render state in [`CoreSyncState`]. The one other change is
//! that the caller now performs the `snap_cell.load()` and passes the resulting `&CoreSnapshot`
//! (rather than this crate taking an `arc_swap` dependency purely to name the cell type). The
//! BODY is unchanged, statement for statement.

use crate::data_sink::{DirectBarStore, TradeStore};
use crate::orderflow::OrderflowAgg;
use crate::tickvol::TickVolAgg;
use crate::workspace::{self, DEFAULT_VENUE};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use vike_chart::{model, DisplayTz};

/// The read-only half of [`sync_from_core`]'s inputs — everything the fold reads but never
/// mutates. Grouped in a struct (rather than passed as eight loose arguments) for the same reason
/// [`tool_views::ToolCtx`](crate::tool_views::ToolCtx) is: the mutable state stays a separate
/// parameter, so the data-in / state-out seam is legible at every call site.
pub struct CoreSyncInputs<'a> {
    /// The frame's `CoreSnapshot`, already loaded off the arc-swap cell by the caller.
    pub snap: &'a vike_core::CoreSnapshot,
    /// Chart/feed keys the GUI has actually subscribed (`App::spawned`).
    pub spawned: &'a HashSet<String>,
    /// Data-manager-deleted keys: the feed still runs, the GUI stops syncing them.
    pub hidden: &'a HashSet<String>,
    /// Global display timezone, re-asserted onto every synced chart (self-healing tz propagation).
    pub display_tz: DisplayTz,
    /// The live trade tape every tick/volume + orderflow aggregator is fed from.
    pub trades: &'a TradeStore,
    /// Background aggTrades-backfill batches (Binance-only by design — see the drain below).
    pub bf_rx: &'a Receiver<(String, Vec<vike_model::TradeTick>)>,
    /// The market-feed status line, shown when the core reports no fault.
    pub feed_status: &'a Mutex<String>,
    /// The direct-bar store (split-plane B2's kline follow-up) — `Some` exactly in the THIRD
    /// MODE ([`split_plane::direct_bars_mount`](crate::split_plane::direct_bars_mount) is the
    /// arm-table pin), `None` in the fat local arm (the core owns klines) and the feed-less
    /// observer. Its presence IS the mode signal the render-source decision takes
    /// (`series_render_source`'s `direct_bars` parameter), so the two cannot disagree.
    pub direct_bars: Option<&'a DirectBarStore>,
}

/// The mutated half: the render model this fold writes into.
pub struct CoreSyncState<'a> {
    /// Per-chart-key render state (`App::charts`).
    pub charts: &'a mut HashMap<String, model::ChartState>,
    /// Tick/volume aggregators, `chart key -> (venue, symbol, agg)`.
    pub aggs: &'a mut HashMap<String, (String, String, TickVolAgg)>,
    /// Orderflow (footprint/CVD) aggregators, `chart key -> (venue, symbol, agg)`.
    pub of_aggs: &'a mut HashMap<String, (String, String, OrderflowAgg)>,
    /// **Undelivered aggTrades-backfill ticks, `symbol -> ticks`** (module-doc bug 4). A batch
    /// drained off `bf_rx` at a frame where `of_aggs` holds no `(DEFAULT_VENUE, symbol)` entry is
    /// staged here instead of being dropped, and applied on the first later frame that does have
    /// one. Empty in steady state: the normal path stages and delivers a batch within the same
    /// call, so an entry surviving a frame means the chart is genuinely absent right now.
    pub bf_pending: &'a mut HashMap<String, Vec<vike_model::TradeTick>>,
    /// Last `CoreSnapshot.seq` folded into `charts` — the single GLOBAL dirty flag.
    pub last_seq: &'a mut u64,
    /// Last [`DirectBarStore::generation`] folded into `charts` — the direct-bar fold's own
    /// dirty flag, `last_seq`'s store-side twin. A pure monotonic fold cursor: nothing ever
    /// resets it (a backend switch keeps the store painting, and the feed-plane teardown's
    /// `clear` BUMPS the generation, so the fold refolds the emptied store on its own).
    pub last_direct_gen: &'a mut u64,
    /// The status line the GUI paints.
    pub status: &'a mut String,
}

/// Per-symbol ceiling on [`CoreSyncState::bf_pending`], in ticks — the one place staged backfill
/// can still lose data, and it `warn!`s when it does.
///
/// Deliberately well ABOVE what the producer can emit: `maybe_spawn_backfill` calls
/// `vike_binance::agg_trades_backfill_reported(…, max_pages = 300, …)` with each page at
/// `limit=1000`, so ONE walk emits ≤ 300_000 ticks — and a symbol only ever gets one walk's worth,
/// even now that `feed_lifecycle::BackfillRetries` can re-walk a failed symbol: a re-walk is
/// claimed ONLY after a report that delivered zero pages, and the first walk that delivers anything
/// ends the chain (see that type's "at most one walk in a chain may deliver" rule). The cap is
/// therefore unreachable today by a factor of ~3; it exists so that raising `max_pages`, or any
/// future backfill that genuinely re-fetches delivered ticks, degrades to a bounded buffer plus a
/// log line rather than to unbounded growth in a long-lived GUI process.
pub const BF_PENDING_MAX_TICKS: usize = 1_000_000;

/// Trim a staged backfill buffer down to `cap` ticks, returning how many were dropped (`0` in
/// every normal case). Drops from the **front**, which is the ticks-nearest-`cap` question that
/// actually matters: `backfill_agg_trades_backward` collects all its pages and then replays them
/// `.rev()`, so the worker emits — and this buffer accumulates — **oldest-first**. Trimming the
/// front therefore truncates the DEEPEST history and keeps the ticks adjacent to the live splice,
/// which are the ones a chart's visible bars can still attach footprints to.
pub fn trim_pending_backfill(held: &mut Vec<vike_model::TradeTick>, cap: usize) -> usize {
    let dropped = held.len().saturating_sub(cap);
    if dropped > 0 {
        held.drain(..dropped);
    }
    dropped
}

/// Fold the latest CoreSnapshot into the render model. The vike-model Bar → render
/// Bar conversion, and the incremental (O(delta), not O(history)) fold, now live in
/// `ChartState::sync` (chart-perf T2) — this loop is just the per-series dispatch.
/// Runs only when the snapshot seq advanced (coalesced by the core, ≤ ~60/s).
pub fn sync_from_core(inp: CoreSyncInputs<'_>, st: CoreSyncState<'_>) {
    let CoreSyncInputs {
        snap,
        spawned,
        hidden,
        display_tz,
        trades,
        bf_rx,
        feed_status,
        direct_bars,
    } = inp;
    let CoreSyncState { charts, aggs, of_aggs, bf_pending, last_seq, last_direct_gen, status } = st;
    // The render-source decision's mode signal: the store's presence (`Some` exactly in the
    // third mode — see `CoreSyncInputs::direct_bars`).
    let direct_mounted = direct_bars.is_some();
    // Core-bar (kline) sync folds only when the CoreSnapshot actually changed (`seq` is the
    // single GLOBAL dirty flag). The tick/volume + orderflow drain further down is DELIBERATELY
    // OUTSIDE this gate (it used to sit inside it): that fold is driven by the live trade tape
    // (`trades`), not the snapshot, so gating it on `seq` meant a workspace with ONLY
    // tick/volume charts — no kline feed to bump `seq` — stalled until unrelated core activity
    // happened to bump it. Draining every frame fixes that; an empty drain is a genuine no-op
    // (empty `aggs`/`of_aggs` maps + a non-blocking `bf_rx.try_iter()`), so the cost is nil.
    if snap.seq != *last_seq {
        *last_seq = snap.seq;
        for ((venue, symbol, interval), series) in &snap.bars {
            // THE DOUBLE-FOLD GUARD (split-plane B2): the snapshot may only fold into series the
            // decision function assigns to it. A tick/volume-interval key folds from the LOCAL
            // trade tape (the drain below), and `spawned` deliberately holds those keys too (it
            // doubles as the ensure gate) — so without this line, a bar series published under a
            // tick/vol interval would repaint a tape-rendered chart from a second source. Never
            // reachable from the local core (its bar cache is kline-only), but the observe modes
            // fold a REMOTE daemon's `WireSnapshot::bars` here, and the third mode runs a live
            // tape — and the direct-bar store — beside it.
            let source = crate::split_plane::series_render_source(direct_mounted, venue, interval);
            if source != crate::split_plane::SeriesSource::SnapshotBars {
                // THE HISTORY SEAM (the direct-bar follow-up): a DIRECT-rendered series never
                // folds from the snapshot, but the backend's streamed tail is offered to the
                // store as its ONE-TIME initial history — `seed_backend_tail` applies only
                // while the series holds no closed bars, so a venue REST seed beats it, a
                // later snapshot can't re-apply it, and live closes append onto it through the
                // store's boundary-ts dedup rule. Without this, a venue whose feed serves no
                // warmup (hyperliquid) would start every kline chart empty.
                if source == crate::split_plane::SeriesSource::DirectBars {
                    let key = workspace::series_key(venue, symbol, interval);
                    if spawned.contains(&key) && !hidden.contains(&key) {
                        if let Some(store) = direct_bars {
                            store.seed_backend_tail(venue, symbol, interval, &series.closed);
                        }
                    }
                }
                continue;
            }
            // Venue-aware key so a non-Binance series (`"venue:SYMBOL@interval"`) matches its own
            // `spawned`/`charts` slot. The core's `SeriesKey` carries the venue; discarding it here
            // (the old `format!("{symbol}@{interval}")`) meant every non-Binance chart's bars fell
            // into the Binance-shaped key and never synced — and any same-named Binance chart wrongly
            // received them. `series_key` is the SAME builder `ensure_feed_on`/`WinState::key` use.
            let key = workspace::series_key(venue, symbol, interval);
            if !spawned.contains(&key) || hidden.contains(&key) {
                continue; // series removed GUI-side (Data-manager delete)
            }
            let Some(cs) = charts.get_mut(&key) else { continue };
            cs.symbol = key.clone();
            // Self-healing tz propagation (task A6): a Cell set is ~free, and this covers both
            // a freshly `or_default()`-ed ChartState (ensure_feed) and any chart created before
            // the last menu tz change — no separate "new chart" hook needed.
            cs.set_tz(display_tz);
            cs.sync(&series.closed, series.forming.as_ref());
        }
    }
    // THE DIRECT-BAR FOLD (the third mode's kline path): the store's series → their charts, on
    // the same every-frame cadence as the tape drain below (the core-free precedent — this fold
    // is driven by the venue feeds, not the snapshot, so the `snap.seq` gate above must not gate
    // it), with the store's own generation as its dirty flag (`snap.seq`'s store-side twin: an
    // unchanged generation means no series changed, so the whole pass is one atomic load).
    // `series_render_source` is consulted per key — the guard is symmetric with the snapshot
    // fold's, so a series can never fold from both stores no matter what lands where.
    if let Some(store) = direct_bars {
        let gen = store.generation();
        if gen != *last_direct_gen {
            *last_direct_gen = gen;
            for (venue, symbol, interval) in store.keys() {
                if crate::split_plane::series_render_source(true, &venue, &interval)
                    != crate::split_plane::SeriesSource::DirectBars
                {
                    continue; // not this fold's series, whatever reached the store
                }
                let key = workspace::series_key(&venue, &symbol, &interval);
                if !spawned.contains(&key) || hidden.contains(&key) {
                    continue; // series removed GUI-side (Data-manager delete)
                }
                let Some(cs) = charts.get_mut(&key) else { continue };
                let Some((closed, forming)) = store.series(&venue, &symbol, &interval) else {
                    continue;
                };
                cs.symbol = key.clone();
                cs.set_tz(display_tz);
                cs.sync(&closed, forming.as_ref());
            }
        }
    }
    // Tick/volume charts (Task B5) AND SP2 orderflow aggregators (Task 7): neither has its
    // own venue kline feed — tick/vol intervals never appear in `snap.bars`, and orderflow
    // needs the RAW trade tape (not just the bar close already synced above) — so both get
    // their own fold here, driven off the trade tape instead of the CoreSnapshot. One
    // `TradeStore::drain` per DISTINCT symbol, shared by EVERY consumer on that symbol
    // (collect both groupings first: iterating `&aggs`/`&of_aggs` while also
    // calling their own `get_mut` in the same loop would conflict) — "one drain, N
    // consumers" holds across both aggregator kinds, not just within `aggs`. `cs.sync` only
    // runs on a non-empty batch — an aggregator's forming bar only changes when trades
    // actually arrived, so an empty drain is a genuine no-op.
    //
    // This fold runs UNCONDITIONALLY (outside the `snap.seq` gate above) because
    // tick/volume/orderflow data never touches the snapshot — it comes from the trade tape. It
    // used to sit behind the gate on the assumption "something always bumps `seq`" (e.g. an open
    // kline chart's forming-bar ticks), but a tick/vol-only workspace has no such feed and
    // stalled. Draining each frame is correct and cheap (an empty drain is a no-op).
    // Venue-aware (Task: venue-aware tick/vol): keyed by (venue, symbol) so two venues'
    // trade tapes for the same symbol (e.g. an OKX `BTC-USDT` alongside a Binance `BTCUSDT`)
    // drain independently and never cross-feed each other's aggregator.
    let mut by_venue_symbol: HashMap<(String, String), Vec<String>> = HashMap::new(); // (venue,symbol) -> chart keys
    for (key, (venue, symbol, _)) in aggs.iter() {
        by_venue_symbol.entry((venue.clone(), symbol.clone())).or_default().push(key.clone());
    }
    // SP2 orderflow: `of_aggs` values carry `(venue, symbol, agg)` (venue-aware), so group by
    // `(venue, symbol)` directly — read from the stored fields, NOT parsed out of the key
    // string. (The old code split the key at the first '@', which mangled a non-Binance key
    // `"venue:SYM@interval"` into the symbol `"venue:SYM"`, so OKX/Bybit orderflow never
    // matched a real (venue, symbol) drain pair.)
    let mut of_by_venue_symbol: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (key, (venue, symbol, _)) in of_aggs.iter() {
        of_by_venue_symbol.entry((venue.clone(), symbol.clone())).or_default().push(key.clone());
    }
    // SP3 Task 3: drain background aggTrades backfill batches BEFORE the live trade drain
    // below. Every backfill id is strictly OLDER than any id the live trades feed has ever
    // emitted (`global-constraints.md`'s no-double-count invariant — enforced by
    // `maybe_spawn_backfill` paging from `earliest_live_ids - 1` downward), so `ingest`ing a
    // backfill batch here and the live batch below (both calls just accumulate into the same
    // per-bar cells — see `OrderflowAgg::ingest`) is equivalent to one `ingest` over their
    // union, even for a "boundary bar" that straddles both. Grouped per symbol first so
    // several queued 2000-trade batches collapse into one `ingest` call per chart key (same
    // "one drain, N consumers" shape as `by_venue_symbol`/`of_by_venue_symbol` above — though
    // today there is exactly one consumer per symbol here: `of_aggs`). Backfill is
    // Binance-only BY DESIGN (only Binance's aggTrades REST paging feeds `bf_rx` — see
    // `maybe_spawn_backfill`'s call site), so a backfill batch for `symbol` only ever targets
    // the Binance orderflow entry `(DEFAULT_VENUE, symbol)`. A non-Binance orderflow chart is
    // live-only (no historical CVD), mirroring how the trade feeds themselves are live-only.
    //
    // **The batch is STAGED, never dropped** (module-doc bug 4). This loop used to `continue`
    // away any batch whose `(DEFAULT_VENUE, symbol)` had no `of_aggs` entry *at the frame it
    // arrived* — and an orderflow aggregator is removed on three ordinary paths (`of_aggs.remove`
    // on a Data-manager delete, `of_aggs.retain(…)` in `reap_orphaned_feeds`, a window whose
    // symbol/interval key changed), any of which can happen while the backfill worker is still
    // paging. The loss was PERMANENT, not merely this-frame: `feed_lifecycle::
    // should_spawn_backfill`'s `bf_spawned` set is insert-only, and the ONE thing that now reopens
    // it (`feed_lifecycle::BackfillRetries`) cannot help here by construction — it re-walks only a
    // symbol whose walk delivered NOTHING, and a batch that reached this loop is delivery. So a
    // page dropped here is still gone for the process, and staging is still the only recovery.
    // Staging costs one already-bounded backfill per symbol (a symbol receives at most ONE walk's
    // worth of ticks — `max_pages = 300` × `limit=1000` — even across a retry chain; see
    // `BackfillRetries`) — the very ticks the UNBOUNDED `bf_rx` channel would otherwise have been
    // holding anyway.
    //
    // "Just leave it on the channel" is not expressible here: `std::sync::mpsc::Receiver` has no
    // peek, so a batch is off the channel the instant its symbol is readable. Staging per symbol
    // one layer up IS that idea, with a cap and a log the channel could not have given us.
    for (symbol, batch) in bf_rx.try_iter() {
        if batch.is_empty() {
            continue;
        }
        // Read BEFORE staging so the log describes the frame the batch arrived on. Emitted per
        // undeliverable BATCH, not per frame: bounded by the ≤ ~150 batches one symbol's
        // run-once backfill can ever produce, so it reports a real fault without becoming spam.
        let deliverable =
            of_by_venue_symbol.contains_key(&(DEFAULT_VENUE.to_string(), symbol.clone()));
        let held = bf_pending.entry(symbol.clone()).or_default();
        held.extend(batch);
        if !deliverable {
            tracing::warn!(
                symbol = %symbol,
                held_ticks = held.len(),
                "orderflow backfill: no aggregator registered for this symbol right now — \
                 holding the batch until one is (re)registered (was: silently discarded)"
            );
        }
        let dropped = trim_pending_backfill(held, BF_PENDING_MAX_TICKS);
        if dropped > 0 {
            tracing::warn!(
                symbol = %symbol,
                dropped_ticks = dropped,
                cap = BF_PENDING_MAX_TICKS,
                "orderflow backfill: staged-tick cap reached — dropped the OLDEST held ticks; \
                 this symbol's historical CVD will be truncated at its far end"
            );
        }
    }
    // Apply every staged symbol that has an aggregator now; keep the rest for a later frame.
    // `retain` is the whole delivery gate: returning `true` holds the ticks, `false` drops the
    // entry only once they have actually been `ingest`ed.
    bf_pending.retain(|symbol, held| {
        let Some(keys) = of_by_venue_symbol.get(&(DEFAULT_VENUE.to_string(), symbol.clone()))
        else {
            return true; // no aggregator this frame — HOLD, never discard
        };
        let mut any_bars = false;
        // Read back off the aggregators we just fed, for the bar-less report below. Both are
        // CUMULATIVE per aggregator (see `OrderflowAgg::evicted_ticks`) and are SUMMED across this
        // symbol's keys because each key holds its OWN copy of the page and can overflow alone.
        let mut pending_now = 0usize;
        let mut evicted = 0u64;
        for key in keys {
            let bar_ots: Vec<i64> =
                charts.get(key).map(|c| c.bars.iter().map(|b| b.ot).collect()).unwrap_or_default();
            any_bars |= !bar_ots.is_empty();
            if let Some((_, _, agg)) = of_aggs.get_mut(key) {
                agg.ingest(held, &bar_ots);
                pending_now += agg.pending_len();
                evicted += agg.evicted_ticks();
            }
        }
        if !any_bars {
            // NOT a lossy path any more (#941). Every key's chart is bar-less this frame, so
            // `OrderflowAgg::ingest` took its EMPTY-grid branch on all of them: the page is HELD
            // in each aggregator's own `pending` and folded in — each tick at ITS OWN bar, by
            // timestamp — on the first later `ingest` that carries a grid. Reachable when the
            // aggregator is registered but its chart has no bars yet (a Data-manager delete
            // removes `charts[key]` alongside `of_aggs[key]`, so a re-add briefly presents an
            // empty grid), which makes the state transient by construction, not a fault.
            //
            // The old objection to holding — "holding for one dead key would starve a second,
            // live key on the same symbol" — no longer applies, because the hold moved INSIDE the
            // aggregator: each key holds only what IT could not place. A MIXED set (one charted
            // key, one bar-less key) therefore places and holds independently, which is why only
            // the all-bar-less case reaches this line — a bar-less key in a mixed set still holds
            // its own copy, and `OrderflowAgg::report_losses` still owns any loss that copy takes.
            //
            // Two paths can still cost ticks, and the aggregator COUNTS both rather than this
            // layer assuming either: eviction at `orderflow::PENDING_MAX_TICKS` (`evicted_ticks`),
            // and a held tick still older than bar 0 when a grid finally appears
            // (`dropped_before_grid`). Only the first is reportable HERE — the second increments
            // on a grid-carrying `ingest`, i.e. a frame where `any_bars` is true and this branch
            // does not run — so it is left to `OrderflowAgg::report_losses`, which owns it. The
            // level is therefore chosen off the counter: `warn!` only once the hold has actually
            // evicted, `debug!` otherwise, so a self-healing handoff stops crying wolf while a
            // real truncation still surfaces. `report_losses` warns on the same eviction but does
            // not know the SYMBOL; this is its attributed twin, not an accidental duplicate, and
            // it is bounded exactly like the staging warn above (≤ ~150 batches per symbol's
            // run-once backfill), never per frame.
            if evicted > 0 {
                tracing::warn!(
                    symbol = %symbol,
                    ticks = held.len(),
                    held_in_agg = pending_now,
                    evicted_ticks = evicted,
                    "orderflow backfill: handed a page to bar-less aggregator(s) whose held-tick \
                     cap has evicted the OLDEST ticks — this symbol's historical CVD is truncated \
                     at its far end"
                );
            } else {
                tracing::debug!(
                    symbol = %symbol,
                    ticks = held.len(),
                    held_in_agg = pending_now,
                    "orderflow backfill: applied a page to an aggregator whose chart has NO bars \
                     yet — OrderflowAgg holds it and places each tick in its own bar once the \
                     grid arrives; nothing is lost"
                );
            }
        }
        false
    });
    // Orderflow is now venue-aware: each `of_aggs` entry carries its own `(venue, symbol)`, so
    // it drains under that pair — the SAME pair a tick/vol chart on that venue+symbol would
    // drain under, preserving "one drain, N consumers" for symbols shared by both aggregator
    // kinds. Union `of_by_venue_symbol` in WITHOUT forcing `DEFAULT_VENUE`, so a non-Binance
    // orderflow chart contributes its real `(venue, symbol)` pair and gets fed from its own tape.
    let all_pairs: HashSet<(String, String)> =
        by_venue_symbol.keys().cloned().chain(of_by_venue_symbol.keys().cloned()).collect();
    for (venue, symbol) in all_pairs {
        let fresh = trades.drain(&venue, &symbol);
        if let Some(keys) = by_venue_symbol.get(&(venue.clone(), symbol.clone())) {
            for key in keys {
                if let Some((_, _, agg)) = aggs.get_mut(key) {
                    agg.ingest(&fresh);
                    if !fresh.is_empty() {
                        if let Some(cs) = charts.get_mut(key) {
                            cs.set_tz(display_tz);
                            cs.sync(&agg.closed, agg.forming().as_ref());
                        }
                    }
                }
            }
        }
        // Feed orderflow for this exact `(venue, symbol)` — no `DEFAULT_VENUE` gate anymore, so
        // OKX/Bybit orderflow gets fed from its own venue's tape.
        if let Some(keys) = of_by_venue_symbol.get(&(venue.clone(), symbol.clone())) {
            for key in keys {
                // Collect `bar_ots` first, as its own statement: `charts` (shared
                // read) and `of_aggs` (mutable) are disjoint bindings so this wouldn't
                // actually conflict even inlined, but keeping the read separate is the
                // defensive shape the brief calls for and reads unambiguously either way.
                let bar_ots: Vec<i64> = charts
                    .get(key)
                    .map(|c| c.bars.iter().map(|b| b.ot).collect())
                    .unwrap_or_default();
                if let Some((_, _, agg)) = of_aggs.get_mut(key) {
                    agg.ingest(&fresh, &bar_ots);
                }
            }
        }
    }
    if let Some(fault) = &snap.fault {
        *status = format!("CORE FAULT: {fault}");
    } else {
        *status = feed_status.lock().unwrap().clone();
    }
}

#[cfg(test)]
mod test_support {
    use super::*;
    use std::sync::mpsc::{channel, Sender};

    /// One `vike_model::Bar` at `ts`, flat at `px`.
    pub fn core_bar(ts: i64, px: f64) -> vike_model::Bar {
        vike_model::Bar {
            ts,
            open: px,
            high: px,
            low: px,
            close: px,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// One closed-only `BarSeries`.
    pub fn series(bars: &[vike_model::Bar]) -> vike_exec::BarSeries {
        vike_exec::BarSeries { closed: std::sync::Arc::new(bars.to_vec()), forming: None }
    }

    /// One aggressive-buy `TradeTick` on `symbol`.
    pub fn trade(symbol: &str, ts: i64, price: f64, size: f64) -> vike_model::TradeTick {
        vike_model::TradeTick {
            ts,
            local_ts: 0,
            price,
            size,
            is_buyer_maker: false,
            symbol: symbol.to_string(),
        }
    }

    /// A `ChartState` whose `bars` already carry the given open times — the `bar_ots` grid an
    /// `OrderflowAgg` buckets trades into.
    pub fn chart_with_ots(ots: &[i64]) -> model::ChartState {
        let mut cs = model::ChartState::default();
        cs.bars.extend(ots.iter().enumerate().map(|(i, &ot)| model::Bar {
            t: i as f64,
            ot,
            o: 1.0,
            h: 1.0,
            l: 1.0,
            c: 1.0,
            v: 0.0,
        }));
        cs
    }

    /// Total buy+sell volume an `OrderflowAgg` has booked across every bar/bucket — the single
    /// number that says "did this aggregator receive the trades".
    pub fn of_total(agg: &OrderflowAgg) -> f64 {
        agg.footprints().iter().flat_map(|b| b.cells.iter()).map(|c| c.buy_vol + c.sell_vol).sum()
    }

    pub fn keys(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The whole mutable half, owned, so a test can call [`sync`] repeatedly against it.
    #[derive(Default)]
    pub struct State {
        pub charts: HashMap<String, model::ChartState>,
        pub aggs: HashMap<String, (String, String, TickVolAgg)>,
        pub of_aggs: HashMap<String, (String, String, OrderflowAgg)>,
        pub bf_pending: HashMap<String, Vec<vike_model::TradeTick>>,
        pub last_seq: u64,
        pub last_direct_gen: u64,
        pub status: String,
    }

    /// Both ends of the backfill lane — a type alias so [`bf_channel`]'s return stays under
    /// `clippy::type_complexity`'s threshold (the pair of generics over the same nested tuple
    /// scores well past it inline).
    pub type BfChannel = (
        Sender<(String, Vec<vike_model::TradeTick>)>,
        Receiver<(String, Vec<vike_model::TradeTick>)>,
    );

    /// A fresh, never-fed backfill channel — the "no backfill this frame" default. The `Sender`
    /// is returned so a test that DOES want backfill batches can push before calling [`sync`];
    /// holding it also keeps `try_iter` from being a disconnected no-op by accident.
    pub fn bf_channel() -> BfChannel {
        channel()
    }

    /// Run the real fold. `spawned`/`hidden` and the feed-status line are the only knobs a test
    /// usually needs beyond `State` itself. The direct-bar store is unmounted (`None`) — the fat
    /// local / thin observe shape; [`sync_direct`] is the third-mode twin.
    pub fn sync(
        st: &mut State,
        snap: &vike_core::CoreSnapshot,
        spawned: &HashSet<String>,
        hidden: &HashSet<String>,
        trades: &TradeStore,
        bf_rx: &Receiver<(String, Vec<vike_model::TradeTick>)>,
        feed_status: &Mutex<String>,
    ) {
        sync_with(st, snap, spawned, hidden, trades, bf_rx, feed_status, None);
    }

    /// [`sync`] with the direct-bar store MOUNTED — the third-mode shape.
    #[allow(clippy::too_many_arguments)]
    pub fn sync_direct(
        st: &mut State,
        snap: &vike_core::CoreSnapshot,
        spawned: &HashSet<String>,
        hidden: &HashSet<String>,
        trades: &TradeStore,
        bf_rx: &Receiver<(String, Vec<vike_model::TradeTick>)>,
        feed_status: &Mutex<String>,
        direct_bars: &DirectBarStore,
    ) {
        sync_with(st, snap, spawned, hidden, trades, bf_rx, feed_status, Some(direct_bars));
    }

    #[allow(clippy::too_many_arguments)]
    fn sync_with(
        st: &mut State,
        snap: &vike_core::CoreSnapshot,
        spawned: &HashSet<String>,
        hidden: &HashSet<String>,
        trades: &TradeStore,
        bf_rx: &Receiver<(String, Vec<vike_model::TradeTick>)>,
        feed_status: &Mutex<String>,
        direct_bars: Option<&DirectBarStore>,
    ) {
        sync_from_core(
            CoreSyncInputs {
                snap,
                spawned,
                hidden,
                display_tz: DisplayTz::Utc,
                trades,
                bf_rx,
                feed_status,
                direct_bars,
            },
            CoreSyncState {
                charts: &mut st.charts,
                aggs: &mut st.aggs,
                of_aggs: &mut st.of_aggs,
                bf_pending: &mut st.bf_pending,
                last_seq: &mut st.last_seq,
                last_direct_gen: &mut st.last_direct_gen,
                status: &mut st.status,
            },
        );
    }

    /// Register a Binance orderflow aggregator (plus the chart whose `bar_ots` grid it buckets
    /// into) under `key` — the "the orderflow chart is open" half of the close/reopen scenarios.
    /// A FRESH `OrderflowAgg` every call, exactly like `vike-app`'s `of_aggs.entry(key)
    /// .or_insert_with(…)` produces after the old entry was reaped.
    pub fn open_orderflow(st: &mut State, key: &str, symbol: &str, ots: &[i64]) {
        st.charts.insert(key.to_string(), chart_with_ots(ots));
        st.of_aggs.insert(
            key.to_string(),
            (DEFAULT_VENUE.to_string(), symbol.to_string(), OrderflowAgg::new(0.5)),
        );
    }

    /// The Data-manager delete / `reap_orphaned_feeds` teardown, as the fold sees it: the
    /// aggregator entry is gone while the backfill worker is still paging.
    pub fn close_orderflow(st: &mut State, key: &str) {
        st.of_aggs.remove(key);
    }
}

/// **Regression pins for the four silent-data-loss bugs this fold has already had.** Each test
/// asserts today's (correct) behaviour at one of the four sites named in the module doc — the
/// behaviour that, while this function lived in the CI-excluded `main.rs`, NOTHING could check.
/// A regression to any of the four old shapes turns one of these red instead of quietly losing a
/// venue's bars, a venue's orderflow, a whole tick/volume workspace's updates, or an orderflow
/// chart's entire historical CVD.
#[cfg(test)]
mod bug_pin_tests {
    use super::test_support::*;
    use super::*;
    use crate::tickvol::BarKind;
    use vike_core::CoreSnapshot;

    /// **BUG 1 PIN** — a non-Binance series must fold into ITS OWN venue-prefixed `charts` slot,
    /// and must NOT reach a same-named Binance chart. The old fold keyed with
    /// `format!("{symbol}@{interval}")`, which sent every venue's bars to the Binance-shaped key:
    /// the OKX chart stayed empty forever and the Binance chart got OKX's candles.
    #[test]
    fn bug1_pin_non_binance_bars_fold_into_the_venue_prefixed_key_not_the_binance_one() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.seq = 1;
        snap.bars.insert(
            ("okx".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
            series(&[core_bar(1_000, 10.0), core_bar(2_000, 11.0)]),
        );

        let mut st = State::default();
        // Both charts exist and are both subscribed; only the OKX one may receive these bars.
        st.charts.insert("okx:BTCUSDT@1m".to_string(), model::ChartState::default());
        st.charts.insert("BTCUSDT@1m".to_string(), model::ChartState::default());
        let spawned = keys(&["okx:BTCUSDT@1m", "BTCUSDT@1m"]);
        let (_tx, rx) = bf_channel();
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());

        sync(&mut st, &snap, &spawned, &HashSet::new(), &trades, &rx, &fs);

        assert_eq!(
            st.charts["okx:BTCUSDT@1m"].bars.len(),
            2,
            "the OKX series must fold into the venue-prefixed key"
        );
        assert_eq!(
            st.charts["BTCUSDT@1m"].bars.len(),
            0,
            "the Binance chart must NOT receive the OKX venue's bars (the old venue-discarding key)"
        );
        assert_eq!(
            st.charts["okx:BTCUSDT@1m"].symbol, "okx:BTCUSDT@1m",
            "ChartState::symbol carries the full venue-aware series key"
        );
    }

    /// **BUG 2 PIN** — orderflow must group by the `(venue, symbol)` STORED in the `of_aggs`
    /// value, never by parsing the chart key. The old code split the key at the first `'@'`,
    /// turning `"okx:BTCUSDT@1m"` into the symbol `"okx:BTCUSDT"` — a pair the `TradeStore` never
    /// holds — so every non-Binance orderflow chart silently received nothing, forever.
    #[test]
    fn bug2_pin_orderflow_groups_by_the_stored_venue_symbol_not_by_splitting_the_key_at_at() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut st = State::default();
        st.charts.insert("okx:BTCUSDT@1m".to_string(), chart_with_ots(&[1_000]));
        st.of_aggs.insert(
            "okx:BTCUSDT@1m".to_string(),
            ("okx".to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
        );

        let trades = TradeStore::default();
        trades.push("okx", &trade("BTCUSDT", 2_000, 10.0, 3.0));
        let (_tx, rx) = bf_channel();
        let fs = Mutex::new(String::new());

        sync(&mut st, &snap, &keys(&["okx:BTCUSDT@1m"]), &HashSet::new(), &trades, &rx, &fs);

        let agg = &st.of_aggs["okx:BTCUSDT@1m"].2;
        assert_eq!(
            of_total(agg),
            3.0,
            "the OKX orderflow agg must be fed from the ('okx','BTCUSDT') tape it stores"
        );
        assert!(agg.generation() > 0, "the footprint cache must have actually rebuilt");
    }

    /// **BUG 2 PIN (the other half)** — the two venues' tapes for the SAME symbol stay separate:
    /// a Binance trade must never reach the OKX aggregator, and vice versa. The key-splitting bug
    /// and any future "just use the symbol" shortcut both break this.
    #[test]
    fn bug2_pin_two_venues_same_symbol_orderflow_never_cross_feed() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut st = State::default();
        for (key, venue) in [("okx:BTCUSDT@1m", "okx"), ("BTCUSDT@1m", "binance")] {
            st.charts.insert(key.to_string(), chart_with_ots(&[1_000]));
            st.of_aggs.insert(
                key.to_string(),
                (venue.to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
            );
        }

        let trades = TradeStore::default();
        trades.push("okx", &trade("BTCUSDT", 2_000, 10.0, 3.0));
        trades.push("binance", &trade("BTCUSDT", 2_000, 10.0, 7.0));
        let (_tx, rx) = bf_channel();
        let fs = Mutex::new(String::new());

        sync(
            &mut st,
            &snap,
            &keys(&["okx:BTCUSDT@1m", "BTCUSDT@1m"]),
            &HashSet::new(),
            &trades,
            &rx,
            &fs,
        );

        assert_eq!(of_total(&st.of_aggs["okx:BTCUSDT@1m"].2), 3.0, "OKX gets only OKX's tape");
        assert_eq!(of_total(&st.of_aggs["BTCUSDT@1m"].2), 7.0, "Binance gets only Binance's tape");
    }

    /// **BUG 3 PIN** — the tick/volume + orderflow drain must run even when `snap.seq` did NOT
    /// advance. It used to sit inside the `seq` gate, so a workspace with ONLY tick/volume charts
    /// (nothing bumps `seq`) stalled: bars stopped forming until unrelated core activity happened
    /// to publish a new snapshot. The same test also pins that the KLINE half is still gated —
    /// a spawned+charted kline series in the snapshot must NOT re-fold on an unchanged `seq`.
    #[test]
    fn bug3_pin_tick_volume_drain_runs_even_when_the_snapshot_seq_did_not_advance() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.seq = 7;
        snap.bars.insert(
            ("binance".to_string(), "ETHUSDT".to_string(), "1m".to_string()),
            series(&[core_bar(1_000, 10.0)]),
        );

        // `last_seq == snap.seq`: this snapshot is already folded, so the kline gate is CLOSED
        // for this frame — the whole point of the pin.
        let mut st = State { last_seq: 7, ..Default::default() };
        st.charts.insert("ETHUSDT@1m".to_string(), model::ChartState::default());
        st.charts.insert("BTCUSDT@2t".to_string(), model::ChartState::default());
        st.aggs.insert(
            "BTCUSDT@2t".to_string(),
            (
                "binance".to_string(),
                "BTCUSDT".to_string(),
                TickVolAgg::new(&BarKind::Tick(2)).expect("Tick(2) is an aggregator kind"),
            ),
        );

        let trades = TradeStore::default();
        for (i, px) in [10.0, 11.0, 12.0].into_iter().enumerate() {
            trades.push("binance", &trade("BTCUSDT", 1_000 + i as i64, px, 1.0));
        }
        let (_tx, rx) = bf_channel();
        let fs = Mutex::new(String::new());

        sync(
            &mut st,
            &snap,
            &keys(&["ETHUSDT@1m", "BTCUSDT@2t"]),
            &HashSet::new(),
            &trades,
            &rx,
            &fs,
        );

        let agg = &st.aggs["BTCUSDT@2t"].2;
        assert_eq!(agg.closed.len(), 1, "2 of the 3 trades must have closed one tick bar");
        assert!(agg.forming().is_some(), "the 3rd trade must be the forming bar");
        assert_eq!(
            st.charts["BTCUSDT@2t"].bars.len(),
            2,
            "the tick chart must be synced (1 closed + 1 forming) on an unchanged-seq frame"
        );
        assert_eq!(
            st.charts["ETHUSDT@1m"].bars.len(),
            0,
            "the KLINE half must still be gated on seq — it may not re-fold on an unchanged seq"
        );
    }

    /// **BUG 4 PIN** — a backfill batch that arrives while the symbol has NO orderflow
    /// aggregator must NOT be discarded. The old fold drained `bf_rx` unconditionally and
    /// `continue`d the batch away; combined with the insert-only `bf_spawned` spawn gate (which
    /// only ever reopens for a walk that delivered NOTHING — never for one whose batch got this
    /// far), that truncated the symbol's historical CVD for the rest of the process.
    #[test]
    fn bug4_pin_a_batch_with_no_aggregator_is_staged_not_discarded() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut st = State::default(); // no `of_aggs` entry: the window was closed/reaped
        let (tx, rx) = bf_channel();
        tx.send(("BTCUSDT".to_string(), vec![trade("BTCUSDT", 1_500, 10.0, 5.0)])).unwrap();
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());

        sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);

        assert_eq!(
            st.bf_pending.get("BTCUSDT").map(|v| v.len()),
            Some(1),
            "the batch must be HELD for a later frame, not dropped on the floor"
        );
    }
}

/// Behaviour tests for the rest of the fold — the gates, the drain-sharing contract and the
/// status line. Same rationale as [`bug_pin_tests`]: none of this ran in any gate before the move.
#[cfg(test)]
mod fold_tests {
    use super::test_support::*;
    use super::*;
    use crate::tickvol::BarKind;
    use vike_core::CoreSnapshot;

    /// A series whose key is not in `spawned` (never subscribed GUI-side) or is in `hidden`
    /// (Data-manager delete) is skipped, even though the chart slot exists.
    #[test]
    fn unspawned_and_hidden_series_are_skipped() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.seq = 1;
        for sym in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
            snap.bars.insert(
                ("binance".to_string(), sym.to_string(), "1m".to_string()),
                series(&[core_bar(1_000, 10.0)]),
            );
        }
        let mut st = State::default();
        for sym in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
            st.charts.insert(format!("{sym}@1m"), model::ChartState::default());
        }
        let spawned = keys(&["BTCUSDT@1m", "ETHUSDT@1m"]); // SOLUSDT never subscribed
        let hidden = keys(&["ETHUSDT@1m"]); // ETHUSDT deleted GUI-side
        let (_tx, rx) = bf_channel();
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());

        sync(&mut st, &snap, &spawned, &hidden, &trades, &rx, &fs);

        assert_eq!(st.charts["BTCUSDT@1m"].bars.len(), 1, "spawned + not hidden folds");
        assert_eq!(st.charts["ETHUSDT@1m"].bars.len(), 0, "hidden is skipped");
        assert_eq!(st.charts["SOLUSDT@1m"].bars.len(), 0, "unspawned is skipped");
        assert_eq!(st.last_seq, 1, "the seq is still recorded as folded");
    }

    /// "One drain, N consumers": a tick/volume aggregator and an orderflow aggregator on the
    /// SAME `(venue, symbol)` both see the same batch — the tape is drained once per pair, not
    /// once per consumer (which would give the second consumer nothing).
    #[test]
    fn one_drain_feeds_both_the_tickvol_and_the_orderflow_aggregator_on_a_pair() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut st = State::default();
        st.charts.insert("BTCUSDT@2t".to_string(), model::ChartState::default());
        st.charts.insert("BTCUSDT@1m".to_string(), chart_with_ots(&[1_000]));
        st.aggs.insert(
            "BTCUSDT@2t".to_string(),
            (
                "binance".to_string(),
                "BTCUSDT".to_string(),
                TickVolAgg::new(&BarKind::Tick(2)).expect("Tick(2) is an aggregator kind"),
            ),
        );
        st.of_aggs.insert(
            "BTCUSDT@1m".to_string(),
            ("binance".to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
        );

        let trades = TradeStore::default();
        trades.push("binance", &trade("BTCUSDT", 2_000, 10.0, 2.0));
        trades.push("binance", &trade("BTCUSDT", 2_001, 10.0, 2.0));
        let (_tx, rx) = bf_channel();
        let fs = Mutex::new(String::new());

        sync(
            &mut st,
            &snap,
            &keys(&["BTCUSDT@2t", "BTCUSDT@1m"]),
            &HashSet::new(),
            &trades,
            &rx,
            &fs,
        );

        assert_eq!(st.aggs["BTCUSDT@2t"].2.closed.len(), 1, "tick/vol saw both trades");
        assert_eq!(of_total(&st.of_aggs["BTCUSDT@1m"].2), 4.0, "orderflow saw the SAME two trades");
    }

    /// Backfill batches are Binance-only BY DESIGN: a batch for `symbol` only ever reaches the
    /// `(DEFAULT_VENUE, symbol)` orderflow entry. A non-Binance orderflow chart on the same symbol
    /// is live-only and must receive nothing from the backfill lane.
    #[test]
    fn backfill_batches_reach_only_the_default_venue_orderflow_entry() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut st = State::default();
        for (key, venue) in [("BTCUSDT@1m", DEFAULT_VENUE), ("okx:BTCUSDT@1m", "okx")] {
            st.charts.insert(key.to_string(), chart_with_ots(&[1_000]));
            st.of_aggs.insert(
                key.to_string(),
                (venue.to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
            );
        }

        let (tx, rx) = bf_channel();
        tx.send(("BTCUSDT".to_string(), vec![trade("BTCUSDT", 1_500, 10.0, 5.0)])).unwrap();
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());

        sync(
            &mut st,
            &snap,
            &keys(&["BTCUSDT@1m", "okx:BTCUSDT@1m"]),
            &HashSet::new(),
            &trades,
            &rx,
            &fs,
        );

        assert_eq!(of_total(&st.of_aggs["BTCUSDT@1m"].2), 5.0, "the Binance entry gets the batch");
        assert_eq!(
            of_total(&st.of_aggs["okx:BTCUSDT@1m"].2),
            0.0,
            "a non-Binance orderflow chart is live-only — no backfill"
        );
    }

    /// A core fault outranks the feed-status line; with no fault the status mirrors the feed.
    #[test]
    fn status_shows_the_core_fault_when_set_and_the_feed_status_otherwise() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut st = State::default();
        let (_tx, rx) = bf_channel();
        let trades = TradeStore::default();
        let fs = Mutex::new("binance: connected".to_string());

        sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);
        assert_eq!(st.status, "binance: connected");

        snap.fault = Some("handler panicked".to_string());
        sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);
        assert_eq!(st.status, "CORE FAULT: handler panicked");
    }

    /// The whole fold over empty state is a genuine no-op — the property the "drain every frame"
    /// fix (bug 3) rests on: an empty `aggs`/`of_aggs` plus a non-blocking `try_iter` costs
    /// nothing, so running it unconditionally is free.
    #[test]
    fn an_empty_frame_is_a_no_op() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut st = State::default();
        let (_tx, rx) = bf_channel();
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());
        sync(&mut st, &snap, &HashSet::new(), &HashSet::new(), &trades, &rx, &fs);
        assert!(st.charts.is_empty() && st.aggs.is_empty() && st.of_aggs.is_empty());
        assert!(st.bf_pending.is_empty(), "an empty frame stages nothing");
        assert_eq!(st.last_seq, 0, "seq 0 == last_seq 0 leaves the gate closed");
    }

    /// **THE DOUBLE-FOLD GUARD (split-plane B2)** — a series present in BOTH `snap.bars` and the
    /// local tick path renders from exactly ONE source, decided by
    /// `split_plane::series_render_source`. A tick-interval key (`"…@100t"`) is tape-rendered:
    /// its chart folds from the `TickVolAgg` drain and the snapshot fold must SKIP a bar series a
    /// (remote) snapshot publishes under that key — while a kline key in the same snapshot still
    /// folds normally. Without the guard the same `ChartState` would be `sync`ed from two sources
    /// in one frame, which is the exact bug class `clear_session_state`'s clear-list exists for.
    #[test]
    fn a_snapshot_bar_series_under_a_tick_interval_never_repaints_a_tape_rendered_chart() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.seq = 1;
        snap.bars.insert(
            ("binance".to_string(), "BTCUSDT".to_string(), "100t".to_string()),
            series(&[core_bar(1_000, 10.0), core_bar(2_000, 11.0)]),
        );
        snap.bars.insert(
            ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
            series(&[core_bar(1_000, 10.0)]),
        );

        let mut st = State::default();
        st.charts.insert("BTCUSDT@100t".to_string(), model::ChartState::default());
        st.charts.insert("BTCUSDT@1m".to_string(), model::ChartState::default());
        st.aggs.insert(
            "BTCUSDT@100t".to_string(),
            (
                "binance".to_string(),
                "BTCUSDT".to_string(),
                crate::tickvol::TickVolAgg::new(&BarKind::Tick(100)).expect("tick agg"),
            ),
        );
        let spawned = keys(&["BTCUSDT@100t", "BTCUSDT@1m"]);
        let (_tx, rx) = bf_channel();
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());

        sync(&mut st, &snap, &spawned, &HashSet::new(), &trades, &rx, &fs);

        assert_eq!(
            st.charts["BTCUSDT@100t"].bars.len(),
            0,
            "the tick-interval chart is TAPE-rendered — snapshot bars under its key are skipped"
        );
        assert_eq!(
            st.charts["BTCUSDT@1m"].bars.len(),
            1,
            "the kline series in the same snapshot still folds — the guard is per series"
        );
    }
}

/// The aggTrades-backfill staging lane (module-doc bug 4) — the live data-loss this fold used to
/// have: `bf_rx` was drained unconditionally and any batch with no `of_aggs` entry to land in was
/// `continue`d away, while the spawn gate guaranteed it would never be refetched. These tests own
/// the three properties the fix has to hold: nothing is lost while the chart is gone, everything
/// that was held arrives once it returns, and the run-once spawn guard does not stand in the way.
#[cfg(test)]
mod backfill_hold_tests {
    use super::test_support::*;
    use super::*;
    use crate::feed_lifecycle::{should_spawn_backfill, BackfillRetries};
    use vike_core::CoreSnapshot;

    const KEY: &str = "BTCUSDT@1m";
    const SYM: &str = "BTCUSDT";
    const OTS: &[i64] = &[1_000];

    /// One frame of the real fold with an optional backfill batch pushed first. Returns nothing —
    /// every assertion reads `st`.
    fn frame(st: &mut State, batch: Option<Vec<vike_model::TradeTick>>) {
        let snap = CoreSnapshot::empty(DEFAULT_VENUE, SYM);
        let (tx, rx) = bf_channel();
        if let Some(b) = batch {
            tx.send((SYM.to_string(), b)).unwrap();
        }
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());
        sync(st, &snap, &keys(&[KEY]), &HashSet::new(), &trades, &rx, &fs);
    }

    /// The whole reported bug, end to end: the orderflow window goes away mid-backfill (a
    /// Data-manager delete / `reap_orphaned_feeds` sweep — `of_aggs.remove`), several pages land
    /// while it is gone, and the window comes back. Every page must reach the returned
    /// aggregator — the pages are the ONLY copy, since `bf_spawned` guarantees no refetch.
    #[test]
    fn every_page_that_arrived_while_the_chart_was_gone_reaches_the_aggregator_when_it_returns() {
        let mut st = State::default();
        open_orderflow(&mut st, KEY, SYM, OTS);

        // The window is torn down while the backfill worker is still paging.
        close_orderflow(&mut st, KEY);
        for size in [1.0, 2.0, 4.0] {
            frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, size)]));
        }
        assert_eq!(
            st.bf_pending[SYM].len(),
            3,
            "all three pages must be held while no aggregator exists"
        );

        // Orderflow re-enabled: `vike-app`'s `of_aggs.entry(..).or_insert_with(..)` builds a FRESH
        // aggregator, so the staged pages are the only history it can ever get.
        open_orderflow(&mut st, KEY, SYM, OTS);
        frame(&mut st, None);

        assert_eq!(
            of_total(&st.of_aggs[KEY].2),
            7.0,
            "the returned aggregator must hold EVERY tick staged while it was gone (1+2+4)"
        );
        assert!(st.bf_pending.is_empty(), "delivered pages are released from the staging buffer");
    }

    /// The run-once spawn guard (`feed_lifecycle::should_spawn_backfill`'s insert-only
    /// `bf_spawned`) still refuses to re-spawn on a re-enable — that is deliberate, and it is
    /// exactly why dropping a page was permanent. Recovery must therefore come from the staging
    /// buffer, WITHOUT a refetch: the guard may not stand between the held pages and the chart.
    ///
    /// `BackfillRetries` does not change this, and the walk scripted here is the reason: it
    /// DELIVERED (the batch below reached `bf_rx`), and a delivering walk never re-walks — so the
    /// guard is as shut here as it ever was, and staging is still the only recovery.
    #[test]
    fn the_run_once_spawn_guard_does_not_prevent_recovery_of_held_pages() {
        let mut bf_spawned: HashSet<String> = HashSet::new();
        let mut retries = BackfillRetries::default();
        assert!(
            should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, SYM),
            "first enable spawns"
        );

        let mut st = State::default();
        open_orderflow(&mut st, KEY, SYM, OTS);
        close_orderflow(&mut st, KEY);
        frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, 5.0)]));

        // Re-enable: the guard says "already spawned", so NOTHING will refetch these ticks.
        open_orderflow(&mut st, KEY, SYM, OTS);
        assert!(
            !should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, SYM),
            "the run-once guard is still closed — there is no second fetch to fall back on"
        );
        frame(&mut st, None);

        assert_eq!(
            of_total(&st.of_aggs[KEY].2),
            5.0,
            "recovery must come from the staged pages, not from a refetch the guard forbids"
        );
    }

    /// Staging is invisible on the happy path: with an aggregator present the batch is applied in
    /// the SAME call and leaves no entry behind — so a steady-state process holds nothing.
    #[test]
    fn a_batch_with_an_aggregator_present_is_applied_in_the_same_frame_and_leaves_nothing_staged() {
        let mut st = State::default();
        open_orderflow(&mut st, KEY, SYM, OTS);
        frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, 5.0)]));

        assert_eq!(of_total(&st.of_aggs[KEY].2), 5.0);
        assert!(st.bf_pending.is_empty(), "nothing is held once it has been ingested");
    }

    /// Delivery must be exactly-once: a staged page is released only after `ingest`, and later
    /// frames must not re-apply it (double-counted CVD is as wrong as missing CVD).
    #[test]
    fn a_delivered_page_is_never_ingested_twice() {
        let mut st = State::default();
        open_orderflow(&mut st, KEY, SYM, OTS);
        close_orderflow(&mut st, KEY);
        frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, 5.0)]));
        open_orderflow(&mut st, KEY, SYM, OTS);

        frame(&mut st, None);
        frame(&mut st, None);
        frame(&mut st, None);
        assert_eq!(of_total(&st.of_aggs[KEY].2), 5.0, "three more frames must not re-ingest it");
    }

    /// A non-Binance orderflow chart is live-only by design, so it is NOT an aggregator that can
    /// take a backfill batch: the page stays staged (and warned about) rather than being applied
    /// to the wrong venue or discarded.
    #[test]
    fn a_non_binance_aggregator_does_not_satisfy_a_staged_binance_page() {
        let mut st = State::default();
        st.charts.insert("okx:BTCUSDT@1m".to_string(), chart_with_ots(OTS));
        st.of_aggs.insert(
            "okx:BTCUSDT@1m".to_string(),
            ("okx".to_string(), SYM.to_string(), OrderflowAgg::new(0.5)),
        );
        frame(&mut st, Some(vec![trade(SYM, 1_500, 10.0, 5.0)]));

        assert_eq!(of_total(&st.of_aggs["okx:BTCUSDT@1m"].2), 0.0, "still live-only");
        assert_eq!(st.bf_pending[SYM].len(), 1, "the page is held for a Binance aggregator");
    }

    /// An empty batch is not a fault and must not create a staging entry (the worker's final
    /// `send` is guarded, but the lane should not depend on that).
    #[test]
    fn an_empty_batch_stages_nothing() {
        let mut st = State::default();
        frame(&mut st, Some(Vec::new()));
        assert!(st.bf_pending.is_empty());
    }

    /// [`trim_pending_backfill`] is a no-op at or below the cap — the only case production ever
    /// reaches, since a symbol's whole backfill is ≤ 300 pages × 1000 ticks.
    #[test]
    fn trim_is_a_no_op_at_or_below_the_cap() {
        let mut held: Vec<vike_model::TradeTick> =
            (0..4).map(|i| trade(SYM, 1_000 + i, 10.0, 1.0)).collect();
        assert_eq!(trim_pending_backfill(&mut held, 4), 0, "exactly at the cap keeps everything");
        assert_eq!(held.len(), 4);
        assert_eq!(trim_pending_backfill(&mut held, 10), 0);
        assert_eq!(held.len(), 4);
    }

    /// Over the cap, the trim drops from the FRONT: the worker emits oldest-first (its pager
    /// replays collected pages `.rev()`), so the surviving tail is the history nearest the live
    /// splice — the part a chart's visible bars can still attach footprints to.
    #[test]
    fn trim_over_the_cap_drops_the_oldest_ticks_and_keeps_the_newest() {
        let mut held: Vec<vike_model::TradeTick> =
            (0..5).map(|i| trade(SYM, 1_000 + i, 10.0, 1.0)).collect();
        assert_eq!(trim_pending_backfill(&mut held, 2), 3, "3 of 5 dropped");
        assert_eq!(
            held.iter().map(|t| t.ts).collect::<Vec<_>>(),
            vec![1_003, 1_004],
            "the two NEWEST ticks survive"
        );
    }
}

/// The DIRECT-BAR fold (split-plane B2's kline follow-up): the third mode's kline charts render
/// from the venue-fed [`DirectBarStore`], never from `snap.bars` — the render-source uniqueness
/// family, driven through the REAL fold with BOTH inputs populated. Plus the history seam (the
/// backend tail as a one-time seed) and the fold's cadence/self-healing contracts.
#[cfg(test)]
mod direct_bar_tests {
    use super::test_support::*;
    use super::*;
    use vike_core::CoreSnapshot;

    const KEY: &str = "BTCUSDT@1m";

    /// One third-mode frame: the real fold with the store MOUNTED.
    fn frame(
        st: &mut State,
        snap: &CoreSnapshot,
        spawned: &HashSet<String>,
        store: &DirectBarStore,
    ) {
        let (_tx, rx) = bf_channel();
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());
        sync_direct(st, snap, spawned, &HashSet::new(), &trades, &rx, &fs, store);
    }

    /// THE UNIQUENESS PIN, direct-bar edition — both inputs driven, one wins: a binance kline
    /// series present in `snap.bars` AND in the store paints from the STORE, and a later
    /// snapshot bump (with the direct fold quiescent — store generation unchanged) must NOT
    /// repaint it from the snapshot: the seq-gated kline fold really skips the series, rather
    /// than being papered over by the direct fold running afterwards.
    #[test]
    fn a_direct_bars_series_never_folds_from_snap_bars_even_when_both_carry_it() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.seq = 1;
        snap.bars.insert(
            ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
            series(&[core_bar(60_000, 10.0), core_bar(120_000, 10.0), core_bar(180_000, 10.0)]),
        );
        let store = DirectBarStore::default();
        store.seed(
            "binance",
            "BTCUSDT",
            "1m",
            vec![core_bar(60_000, 99.0), core_bar(120_000, 99.0)],
        );

        let mut st = State::default();
        st.charts.insert(KEY.to_string(), model::ChartState::default());
        let spawned = keys(&[KEY]);

        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts[KEY].bars.len(), 2, "the chart paints the STORE's two bars");
        assert_eq!(st.charts[KEY].bars[0].o, 99.0, "…with the store's prices, not the snapshot's");
        let (closed, _) = store.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(closed[0].close, 99.0, "a non-empty series refuses the backend tail");

        // The snapshot bumps; the store does not. A broken guard would repaint 3 backend bars.
        snap.seq = 2;
        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts[KEY].bars.len(), 2, "the snapshot fold must SKIP the DirectBars key");
        assert_eq!(st.charts[KEY].bars[0].o, 99.0);
    }

    /// The pre-existing modes are unchanged: with the store UNMOUNTED (fat local / thin observe
    /// — `sync`, not `sync_direct`) the same snapshot folds the kline series exactly as before.
    #[test]
    fn without_the_store_mounted_the_same_snapshot_folds_as_before() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.seq = 1;
        snap.bars.insert(
            ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string()),
            series(&[core_bar(60_000, 10.0), core_bar(120_000, 11.0)]),
        );
        let mut st = State::default();
        st.charts.insert(KEY.to_string(), model::ChartState::default());
        let (_tx, rx) = bf_channel();
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());

        sync(&mut st, &snap, &keys(&[KEY]), &HashSet::new(), &trades, &rx, &fs);
        assert_eq!(st.charts[KEY].bars.len(), 2, "unmounted store: snapshot klines fold as ever");
        assert_eq!(st.charts[KEY].bars[0].o, 10.0);
    }

    /// A kline series on a venue WITHOUT a local bar feed (polymarket refuses `subscribe_bars`;
    /// deribit mounts no local feed at all) keeps the backend tail in the third mode — folded
    /// from `snap.bars`, with nothing seeded into the store for it.
    #[test]
    fn a_venue_without_a_local_bar_feed_keeps_the_snapshot_tail_in_the_third_mode() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.seq = 1;
        for venue in ["polymarket", "deribit"] {
            snap.bars.insert(
                (venue.to_string(), "X".to_string(), "1m".to_string()),
                series(&[core_bar(60_000, 10.0), core_bar(120_000, 11.0)]),
            );
        }
        let store = DirectBarStore::default();
        let mut st = State::default();
        st.charts.insert("polymarket:X@1m".to_string(), model::ChartState::default());
        st.charts.insert("deribit:X@1m".to_string(), model::ChartState::default());
        let spawned = keys(&["polymarket:X@1m", "deribit:X@1m"]);

        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts["polymarket:X@1m"].bars.len(), 2, "backend tail renders");
        assert_eq!(st.charts["deribit:X@1m"].bars.len(), 2, "backend tail renders");
        assert!(store.keys().is_empty(), "nothing is seeded into the store for snapshot venues");
    }

    /// THE HISTORY SEAM, end to end: a direct series whose venue feed serves no warmup
    /// (hyperliquid) is seeded ONCE from the backend's streamed tail, live closes then append
    /// through the store's boundary-ts dedup rule, and a later snapshot cannot re-apply the
    /// tail — the boundary bar appears exactly once, updated to the venue's own close.
    #[test]
    fn the_backend_tail_seeds_an_empty_direct_series_once_then_live_bars_append() {
        let hl_key = "hyperliquid:BTC@1m";
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.seq = 1;
        snap.bars.insert(
            ("hyperliquid".to_string(), "BTC".to_string(), "1m".to_string()),
            series(&[core_bar(60_000, 10.0), core_bar(120_000, 11.0)]),
        );
        let store = DirectBarStore::default();
        let mut st = State::default();
        st.charts.insert(hl_key.to_string(), model::ChartState::default());
        let spawned = keys(&[hl_key]);

        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts[hl_key].bars.len(), 2, "the tail seeds the empty series and paints");

        // The venue re-closes the boundary window, then closes the next — the live splice.
        store.close("hyperliquid", "BTC", "1m", core_bar(120_000, 11.5));
        store.close("hyperliquid", "BTC", "1m", core_bar(180_000, 12.0));
        frame(&mut st, &snap, &spawned, &store); // seq unchanged — the tape-cadence fold paints
        assert_eq!(
            st.charts[hl_key].bars.len(),
            3,
            "the boundary bar appears exactly ONCE (replaced, never duplicated) and the next \
             bar appends — the double-paint class this seam exists to prevent"
        );
        assert_eq!(st.charts[hl_key].bars[2].c, 12.0, "the live splice renders");
        // The DATA plane holds the venue's own close at the boundary. (The RENDER of that one
        // bar keeps the seeded value until any full rebuild: `ChartState::sync`'s incremental
        // key is `(len, first_ts, last_ts)`, blind to an in-place same-ts replacement — the
        // SAME trade-off the fat arm's snapshot path has for the core's idempotent re-close. In
        // practice both sides of the boundary are the same venue kline, so the replace is
        // content-identical and the trade-off invisible.)
        let (closed, _) = store.series("hyperliquid", "BTC", "1m").unwrap();
        assert_eq!(closed[1].close, 11.5, "the venue's own close wins the boundary in the store");

        // A later snapshot (same tail) must not re-seed under the accumulated venue bars.
        snap.seq = 2;
        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts[hl_key].bars.len(), 3, "the tail applies ONCE, never again");
        let (closed, _) = store.series("hyperliquid", "BTC", "1m").unwrap();
        assert_eq!(closed[1].close, 11.5, "…and the store still holds the venue's boundary close");
    }

    /// The direct fold runs on the tape cadence, OUTSIDE the `snap.seq` gate (bug-3's precedent):
    /// a store write with an unchanged snapshot repaints the chart on the next frame.
    #[test]
    fn the_direct_fold_runs_even_when_the_snapshot_seq_did_not_advance() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT"); // seq 0 == last_seq 0: gate closed
        let store = DirectBarStore::default();
        store.seed("binance", "BTCUSDT", "1m", vec![core_bar(60_000, 10.0)]);
        let mut st = State::default();
        st.charts.insert(KEY.to_string(), model::ChartState::default());
        let spawned = keys(&[KEY]);

        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts[KEY].bars.len(), 1);
        store.close("binance", "BTCUSDT", "1m", core_bar(120_000, 11.0));
        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts[KEY].bars.len(), 2, "a live close paints without any seq bump");
    }

    /// The GUI-side gates hold for the direct fold exactly as for the snapshot fold: an
    /// unspawned series neither paints nor seeds (no store pollution from the backend tail),
    /// and a hidden one stops painting.
    #[test]
    fn the_direct_fold_respects_spawned_and_hidden() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.seq = 1;
        snap.bars.insert(
            ("binance".to_string(), "ETHUSDT".to_string(), "1m".to_string()),
            series(&[core_bar(60_000, 10.0)]),
        );
        let store = DirectBarStore::default();
        store.seed("binance", "BTCUSDT", "1m", vec![core_bar(60_000, 10.0)]);
        let mut st = State::default();
        st.charts.insert(KEY.to_string(), model::ChartState::default());
        st.charts.insert("ETHUSDT@1m".to_string(), model::ChartState::default());

        // ETHUSDT is in the snapshot but UNSPAWNED: no paint, and no tail seeded into the store.
        frame(&mut st, &snap, &keys(&[KEY]), &store);
        assert_eq!(st.charts[KEY].bars.len(), 1, "the spawned series paints");
        assert_eq!(st.charts["ETHUSDT@1m"].bars.len(), 0, "unspawned: skipped");
        assert!(
            store.series("binance", "ETHUSDT", "1m").is_none(),
            "an unspawned series must not have the backend tail seeded for it"
        );

        // Hidden: the store still holds the series, the chart stops receiving it.
        let mut hidden_st = State::default();
        hidden_st.charts.insert(KEY.to_string(), model::ChartState::default());
        let (_tx, rx) = bf_channel();
        let trades = TradeStore::default();
        let fs = Mutex::new(String::new());
        let hidden = keys(&[KEY]);
        sync_direct(&mut hidden_st, &snap, &keys(&[KEY]), &hidden, &trades, &rx, &fs, &store);
        assert_eq!(hidden_st.charts[KEY].bars.len(), 0, "hidden: skipped");
    }

    /// The generation gate's accepted residual, pinned: a chart entry (re)created AFTER the last
    /// store write stays empty until the store's next write — and that next write (a forming
    /// snapshot, ~1 s on any live symbol; or a fresh seed on re-subscribe) heals it. Same class
    /// as a reopened tick/vol chart waiting for its next trade.
    #[test]
    fn a_recreated_chart_self_heals_on_the_next_store_write() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let store = DirectBarStore::default();
        store.seed("binance", "BTCUSDT", "1m", vec![core_bar(60_000, 10.0)]);
        let mut st = State::default();
        st.charts.insert(KEY.to_string(), model::ChartState::default());
        let spawned = keys(&[KEY]);

        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts[KEY].bars.len(), 1);

        // The window is closed and reopened within the store's quiet period: the fresh entry
        // stays empty this frame (the residual)…
        st.charts.insert(KEY.to_string(), model::ChartState::default());
        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts[KEY].bars.len(), 0, "generation unchanged: the residual, as pinned");

        // …and the venue's next forming frame repaints it.
        store.forming("binance", "BTCUSDT", "1m", core_bar(120_000, 10.5));
        frame(&mut st, &snap, &spawned, &store);
        assert_eq!(st.charts[KEY].bars.len(), 2, "the next store write self-heals the chart");
    }
}
