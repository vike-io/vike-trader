//! `core_sync` — the per-frame `CoreSnapshot` → render-model fold (`sync_from_core`), moved down
//! verbatim out of `vike-app`'s CI-excluded `main.rs`.
//!
//! This is the GUI's whole read side of the R5c seam: the lossy arc-swap `CoreSnapshot` the core
//! publishes plus the live trade tape ([`TradeStore`](crate::data::data_sink::TradeStore)) — and, in
//! the third mode, the venue-fed direct-bar store
//! ([`DirectBarStore`](crate::data::data_sink::DirectBarStore), split-plane B2's kline follow-up) —
//! fold into the per-chart [`ChartState`](vike_chart::model::ChartState)s, the tick/volume
//! aggregators ([`TickVolAgg`](vike_orderflow::tickvol::TickVolAgg)) and the orderflow aggregators
//! ([`OrderflowAgg`](vike_orderflow::bar_agg::OrderflowAgg)). It touches NO egui — every line is map
//! keying, drains and arithmetic — yet it lived in a file no gate compiles, which is exactly why
//! its history is a run of silent-data-loss bugs whose ONLY record is a code comment:
//!
//! 1. **Bar keys dropped the venue** — the fold used `format!("{symbol}@{interval}")`, so every
//!    non-Binance series landed in the Binance-shaped `charts` slot: that chart never synced, and
//!    a same-named Binance chart wrongly received its bars. Fixed by keying with
//!    [`workspace::series_key`](crate::ui::workspace::series_key).
//! 2. **Orderflow grouped by splitting the chart key at `'@'`** — which mangled a non-Binance key
//!    `"venue:SYM@interval"` into the symbol `"venue:SYM"`, so OKX/Bybit orderflow never matched a
//!    real `(venue, symbol)` drain pair and silently received nothing. Fixed by reading the
//!    `(venue, symbol)` STORED in the `of_aggs` value.
//! 3. **The tick/volume + orderflow drain sat inside the `snap.seq` gate** — but that fold is
//!    driven by the trade tape, not the snapshot, so a workspace with ONLY tick/volume charts (no
//!    kline feed to bump `seq`) stalled until unrelated core activity happened to bump it. Fixed
//!    by draining every frame, outside the gate.
//! 4. ⚠ TOMBSTONE — **a drained aggTrades-backfill batch with no aggregator to land in was
//!    DISCARDED**, and per-symbol staging (`bf_pending`, capped, with a `warn!` on the cap) held it
//!    until an aggregator (re)appeared. Both the drain and the staging were DELETED on 2026-10-10
//!    with the lane that fed them: the desktop's Binance aggTrades walk left with the local
//!    market-data plane (rulings 1 and 2), and the batch lane's receiver was built with its sender
//!    already dropped, so no batch could reach this fold — the drain answered `Disconnected` on its
//!    first call every frame and the staging map was empty for the life of every process. A
//!    backfill that returns through the backend must re-earn the hold-never-drop rule this item
//!    records: the run-once spawn gate (`feed_lifecycle::should_spawn_backfill`) still means a page
//!    dropped here is never refetched.
//!
//! The first three are FIXED in the code below; none had a test, because nothing compiled the file
//! they lived in. The `bug_pin_tests` module at the bottom pins today's (correct) behaviour at each
//! of those three sites, so a regression back to any of them fails the merge gate instead of
//! silently losing a venue's data again.
//!
//! **Signature note.** `sync_from_core` was `fn sync_from_core(&mut self)` on `App`. `App` is an
//! eframe type this crate cannot name, so the fields it touched become explicit parameters,
//! grouped exactly the way [`tool_views`](crate::ui::tool_views) already groups them: read-only inputs
//! in [`CoreSyncInputs`], the mutated render state in [`CoreSyncState`]. The one other change is
//! that the caller now performs the `snap_cell.load()` and passes the resulting `&CoreSnapshot`
//! (rather than this crate taking an `arc_swap` dependency purely to name the cell type). The
//! BODY is unchanged, statement for statement.

use crate::data::data_sink::{DirectBarStore, TradeStore};
use crate::ui::workspace;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use vike_chart::{DisplayTz, model};
use vike_orderflow::bar_agg::OrderflowAgg;
use vike_orderflow::tickvol::TickVolAgg;

/// How [`CoreSyncInputs::tape_gaps`] is spelled: a LIVE reader of one `(venue, symbol)`'s tape-gap
/// epoch, called at the point of use rather than sampled into a map beforehand.
///
/// A named alias rather than the bare type, because `clippy::type_complexity` refuses
/// `Option<&dyn Fn(&str, &str) -> u64>` at the one call site that needs the optional form — and
/// because "the epoch source" is what this parameter is, which a name can say and a `dyn Fn`
/// cannot.
pub type TapeGapEpochs<'a> = &'a dyn Fn(&str, &str) -> u64;

/// The read-only half of [`sync_from_core`]'s inputs — everything the fold reads but never
/// mutates. Grouped in a struct (rather than passed as eight loose arguments) for the same reason
/// [`tool_views::ToolCtx`](crate::ui::tool_views::ToolCtx) is: the mutable state stays a separate
/// parameter, so the data-in / state-out seam is legible at every call site.
pub struct CoreSyncInputs<'a> {
    /// The frame's `CoreSnapshot`, already loaded off the arc-swap cell by the caller.
    pub snap: &'a vike_exec::CoreSnapshot,
    /// Chart/feed keys the GUI has actually subscribed (`App::spawned`).
    pub spawned: &'a HashSet<String>,
    /// Data-manager-deleted keys: the feed still runs, the GUI stops syncing them.
    pub hidden: &'a HashSet<String>,
    /// Global display timezone, re-asserted onto every synced chart (self-healing tz propagation).
    pub display_tz: DisplayTz,
    /// The live trade tape every tick/volume + orderflow aggregator is fed from.
    pub trades: &'a TradeStore,
    /// The market-feed status line, shown when the core reports no fault.
    pub feed_status: &'a Mutex<String>,
    /// The GUI-side bar store — `Some` exactly where a plane fills one
    /// ([`split_plane::direct_bars_mount`](crate::backend::split_plane::direct_bars_mount) is the arm-table
    /// pin), `None` in the fat local arm where the core owns klines.
    ///
    /// ⚠ **This is the HANDLE, and no longer the mode SIGNAL.** The signal is [`Self::bar_plane`];
    /// this fold `debug_assert!`s the two agree rather than deriving one from the other. See
    /// [`split_plane::BarPlane`](crate::backend::split_plane::BarPlane) for what the conflation cost.
    pub direct_bars: Option<&'a DirectBarStore>,
    /// **THE RENDER-SOURCE MODE SIGNAL** — what fills [`Self::direct_bars`], and therefore which
    /// series may render from it. Passed to `series_render_source` at both folds below, so the
    /// snapshot half and the store half cannot answer differently about one key.
    pub bar_plane: crate::backend::split_plane::BarPlane,
    /// **A LIVE READER of the per-`(venue, symbol)` TAPE-GAP epoch** — a monotone counter the
    /// market-data producer bumps on every disclosed hole in that key's trade tape
    /// ([`md_session::MdSession::tape_gap_epoch`](crate::data::md_session::MdSession::tape_gap_epoch)),
    /// and the only route a producer has to these aggregators at all: they live on the FRAME thread
    /// and the reader runs on its own.
    ///
    /// ⚠ **It is called AFTER each key's drain, never before — and it is a CALLABLE precisely so
    /// that it cannot be sampled early.** It was a `&HashMap` snapshot, and the shell built that
    /// snapshot as an ARGUMENT EXPRESSION, i.e. strictly before `sync_from_core` was entered, while
    /// the drain it must follow happens hundreds of lines inside. The rule this doc states, the one
    /// at the drain site and the one on
    /// [`TickVolAgg::mark_gap`](vike_orderflow::tickvol::TickVolAgg::mark_gap) were therefore all nominal:
    /// the whole prologue of this fold was a window in which the reader could disclose a hole,
    /// drain the buffered ticks and push post-hole prints, after which this fold took THOSE prints,
    /// read a stale epoch, and folded them into an unrepaired aggregator. The next frame repairs —
    /// but `TickVolAgg::mark_gap` keeps `closed` whole on the argument that every bar in it closed
    /// BEFORE the hole, so a bar that closed inside the mis-folded batch is permanent and unmarked.
    ///
    /// A reader answering `0` for every key is the whole of "no gaps have ever been disclosed",
    /// which is also what a caller with no market-data plane passes.
    pub tape_gaps: TapeGapEpochs<'a>,
}

/// The mutated half: the render model this fold writes into.
pub struct CoreSyncState<'a> {
    /// Per-chart-key render state (`App::charts`).
    pub charts: &'a mut HashMap<String, model::ChartState>,
    /// Tick/volume aggregators, `chart key -> (venue, symbol, agg)`.
    pub aggs: &'a mut HashMap<String, (String, String, TickVolAgg)>,
    /// Orderflow (footprint/CVD) aggregators, `chart key -> (venue, symbol, agg)`.
    pub of_aggs: &'a mut HashMap<String, (String, String, OrderflowAgg)>,
    /// **What this node PUBLISHES**, rewritten from `snap.bars` whenever the snapshot advances —
    /// the one list [`crate::ui::series_follow`]'s three surfaces (adoption, the symbol picker's
    /// backend section, the title-bar badge) all read, so they cannot disagree about what the
    /// backend has.
    ///
    /// It is produced HERE rather than in the shell because this fold already walks `snap.bars`
    /// behind the `seq` gate: computing it at the call site would either re-walk the map every
    /// frame or need a second copy of that gate. Empty until the first snapshot with any series in
    /// it — which is also the honest answer for a node publishing nothing.
    pub published: &'a mut Vec<crate::ui::series_follow::PublishedSeries>,
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

/// **Assign this to [`CoreSyncState::last_seq`] to force ONE refold on the next frame.**
///
/// The kline fold is gated on `snap.seq != *last_seq`, which is right while the render model only
/// ever changes because the snapshot did. It stops being right the moment something ELSE changes
/// which series the GUI wants: `series_follow::follow_backend` retargets a window and subscribes a
/// new key, and until the daemon happens to publish again the already-arrived bars for that key sit
/// in the snapshot unfolded — a chart that adopts correctly and then paints nothing for however
/// long the node's publish cadence is.
///
/// `u64::MAX` rather than `0`: the cursor must differ from the NEXT snapshot's `seq`, and `0` is a
/// real seq (the pre-first-frame `CoreSnapshot::empty`). A counter that reached `u64::MAX` would
/// have had to publish one snapshot per nanosecond for ~584 years.
pub const FORCE_REFOLD: u64 = u64::MAX;

/// One-shot diagnosis line for a bar series that ARRIVED in the snapshot and was NOT rendered.
///
/// ⚠ **A diagnosis surface, not a behaviour change** — every `continue` it annotates is unchanged.
/// Both of them used to be silent, and that silence is how an empty chart could have TWO
/// individually-plausible halves and no place they were ever printed side by side: the daemon
/// publishing one `(venue, symbol, interval)` and the GUI subscribed to another looks, from either
/// end alone, exactly like a daemon with no data. (The daemon-side half of that particular failure
/// is fixed in `crates/vike-tradehub/src/publish.rs`'s `project_bar_series`; this line is what would have found
/// it in a minute rather than a trace.)
///
/// ONCE per `(key, reason)` for the life of the process: this fold runs on the frame thread at
/// repaint cadence, so an unconditional line would be a log flood — and "this series is not
/// rendered" is a standing state rather than an event, so the first line says everything a later
/// one would. Bounded by the number of distinct series a node ever publishes.
///
/// ⚠ **KEEP IT.** [`crate::ui::series_follow`] makes the `not subscribed GUI-side` reason much rarer —
/// a chart nobody chose now adopts a published series instead of dropping it — but it does not make
/// it unreachable, and the cases that remain are exactly the ones worth a line: a chart the
/// operator PINNED to some other series (adoption refuses those by design), a key the Data manager
/// HID, a series arriving while its `ChartState` has not been created yet, and every series beyond
/// the first when a node publishes more of them than the workspace has charts. This line is what
/// found the defect that module fixes; it stays pointed at what is left.
fn note_unrendered_series(key: &str, reason: &str, spawned: &HashSet<String>) {
    static SEEN: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    let tag = format!("{key}\u{1}{reason}");
    let Ok(mut seen) = SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock() else { return };
    if !seen.insert(tag) {
        return;
    }
    drop(seen);
    let mut subscribed: Vec<&str> = spawned.iter().map(String::as_str).collect();
    subscribed.sort_unstable();
    tracing::info!(
        series = key,
        reason = reason,
        subscribed = %subscribed.join(", "),
        "a published bar series is not being rendered"
    );
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
        feed_status,
        direct_bars,
        bar_plane,
        tape_gaps,
    } = inp;
    let CoreSyncState { charts, aggs, of_aggs, published, last_seq, last_direct_gen, status } = st;
    // ⚠ The mode signal is `bar_plane`, NOT `direct_bars.is_some()`. The handle and the plane are
    // separate facts now (see `CoreSyncInputs::bar_plane`), so the pairing is CHECKED here instead
    // of being the definition: a `None` plane with a store handle, or a filling plane with none,
    // is a composition-root bug that would silently reassign every kline series.
    debug_assert_eq!(
        direct_bars.is_some(),
        bar_plane != crate::backend::split_plane::BarPlane::None,
        "the bar-store handle and the bar plane disagree — see split_plane::BarPlane"
    );
    // Core-bar (kline) sync folds only when the CoreSnapshot actually changed (`seq` is the
    // single GLOBAL dirty flag). The tick/volume + orderflow drain further down is DELIBERATELY
    // OUTSIDE this gate (it used to sit inside it): that fold is driven by the live trade tape
    // (`trades`), not the snapshot, so gating it on `seq` meant a workspace with ONLY
    // tick/volume charts — no kline feed to bump `seq` — stalled until unrelated core activity
    // happened to bump it. Draining every frame fixes that; an empty drain is a genuine no-op
    // (empty `aggs`/`of_aggs` maps), so the cost is nil.
    if snap.seq != *last_seq {
        *last_seq = snap.seq;
        // WHAT THIS NODE HAS, refreshed before the per-series dispatch below drops anything. It is
        // deliberately the FULL published set rather than "the ones that failed to render": the
        // symbol picker lists it as reachable rows, so a series that IS rendering must appear
        // there too (as the row already selected), and `series_follow::chart_feed` distinguishes
        // "waiting for a published series" from "subscribed to one nobody publishes" by asking
        // whether this window's own triple is in it.
        *published = crate::ui::series_follow::published_series(snap);
        for ((venue, symbol, interval), series) in &snap.bars {
            // THE DOUBLE-FOLD GUARD (split-plane B2): the snapshot may only fold into series the
            // decision function assigns to it. A tick/volume-interval key folds from the LOCAL
            // trade tape (the drain below), and `spawned` deliberately holds those keys too (it
            // doubles as the ensure gate) — so without this line, a bar series published under a
            // tick/vol interval would repaint a tape-rendered chart from a second source. Never
            // reachable from the local core (its bar cache is kline-only), but the observe modes
            // fold a REMOTE daemon's `WireSnapshot::bars` here, and the third mode runs a live
            // tape — and the direct-bar store — beside it.
            // `published_live: true` by CONSTRUCTION — this loop is iterating `snap.bars`, so the
            // backend demonstrably publishes this exact series this frame. It is spelled as a
            // named binding rather than a bare `true` because that is the whole of the
            // `BackendStore` plane's rule, and a future edit that moved this loop off `snap.bars`
            // would have to answer it rather than inherit it.
            let published_live = true;
            let source = crate::backend::split_plane::series_render_source(
                bar_plane,
                venue,
                interval,
                published_live,
            );
            if source != crate::backend::split_plane::SeriesSource::SnapshotBars {
                // THE HISTORY SEAM (the `VenueFeeds` plane only — under `BackendStore` a
                // published series is `SnapshotBars` by the line above, so this branch is
                // unreachable there and the daemon's tail keeps painting straight from the
                // snapshot, exactly as before the store existed): a DIRECT-rendered series never
                // folds from the snapshot, but the backend's streamed tail is offered to the
                // store as its ONE-TIME initial history — `seed_backend_tail` applies only
                // while the series holds no closed bars, so a venue REST seed beats it, a
                // later snapshot can't re-apply it, and live closes append onto it through the
                // store's boundary-ts dedup rule. Without this, a venue whose feed serves no
                // warmup (hyperliquid) would start every kline chart empty.
                if source == crate::backend::split_plane::SeriesSource::DirectBars {
                    let key = workspace::series_key(venue, symbol, interval);
                    if spawned.contains(&key)
                        && !hidden.contains(&key)
                        && let Some(store) = direct_bars
                    {
                        store.seed_backend_tail(venue, symbol, interval, &series.closed);
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
                note_unrendered_series(
                    &key,
                    if hidden.contains(&key) {
                        "hidden GUI-side (Data-manager delete)"
                    } else {
                        "not subscribed GUI-side (`spawned` holds no such key)"
                    },
                    spawned,
                );
                continue; // series removed GUI-side (Data-manager delete)
            }
            let Some(cs) = charts.get_mut(&key) else {
                note_unrendered_series(
                    &key,
                    "subscribed, but no `ChartState` exists for it",
                    spawned,
                );
                continue;
            };
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
        let generation = store.generation();
        if generation != *last_direct_gen {
            *last_direct_gen = generation;
            for (venue, symbol, interval) in store.keys() {
                // ⚠ The SAME probe the snapshot fold above answers by construction, asked
                // explicitly here because this loop is iterating the STORE: does the backend
                // publish this exact series? Under `BackendStore` a `true` hands the key back to
                // the snapshot fold — which is what keeps a daemon-published 1m series painting
                // live even after somebody's store read landed history under the same key.
                let published_live =
                    snap.bars.contains_key(&(venue.clone(), symbol.clone(), interval.clone()));
                if crate::backend::split_plane::series_render_source(
                    bar_plane,
                    &venue,
                    &interval,
                    published_live,
                ) != crate::backend::split_plane::SeriesSource::DirectBars
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
    // Orderflow is now venue-aware: each `of_aggs` entry carries its own `(venue, symbol)`, so
    // it drains under that pair — the SAME pair a tick/vol chart on that venue+symbol would
    // drain under, preserving "one drain, N consumers" for symbols shared by both aggregator
    // kinds. Union `of_by_venue_symbol` in WITHOUT forcing `DEFAULT_VENUE`, so a non-Binance
    // orderflow chart contributes its real `(venue, symbol)` pair and gets fed from its own tape.
    let all_pairs: HashSet<(String, String)> =
        by_venue_symbol.keys().cloned().chain(of_by_venue_symbol.keys().cloned()).collect();
    for (venue, symbol) in all_pairs {
        let fresh = trades.drain(&venue, &symbol);
        // ⚠ READ THE EPOCH AFTER THE DRAIN — see `CoreSyncInputs::tape_gaps`. A gap disclosed
        // between these two lines belongs to the batch we just took, so consulting it second is
        // what makes `fresh` discardable rather than half-corrupt. It is a CALL and not a lookup
        // into a map the caller handed us, because a map is sampled when it is built and this must
        // be sampled HERE.
        let epoch = tape_gaps(&venue, &symbol);
        if let Some(keys) = by_venue_symbol.get(&(venue.clone(), symbol.clone())) {
            for key in keys {
                if let Some((_, _, agg)) = aggs.get_mut(key) {
                    // A repaired aggregator DISCARDS this frame's batch: it spans the hole. That
                    // costs at most one frame of prints, which the repair is already paying for.
                    let repaired = agg.mark_gap(epoch);
                    if !repaired {
                        agg.ingest(&fresh);
                    }
                    if (!fresh.is_empty() || repaired)
                        && let Some(cs) = charts.get_mut(key)
                    {
                        cs.set_tz(display_tz);
                        cs.sync(&agg.closed, agg.forming().as_ref());
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
                    // The same rule, and the same discard — but the repair is WHOLESALE here
                    // because CVD is cumulative and `cells` is index-aligned with no per-trade
                    // dedup (see `OrderflowAgg::mark_gap`).
                    if !agg.mark_gap(epoch) {
                        agg.ingest(&fresh, &bar_ots);
                    }
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

#[path = "test_support.rs"]
#[cfg(test)]
mod test_support;

/// **Regression pins for the silent-data-loss bugs this fold has already had.** Each test asserts
/// today's (correct) behaviour at one of the three live sites named in the module doc (the fourth
/// is a tombstone) — the behaviour that, while this function lived in the CI-excluded `main.rs`,
/// NOTHING could check. A regression to any of the three old shapes turns one of these red instead
/// of quietly losing a venue's bars, a venue's orderflow or a whole tick/volume workspace's updates.
#[path = "bug_pin_tests.rs"]
#[cfg(test)]
mod bug_pin_tests;

/// Behaviour tests for the rest of the fold — the gates, the drain-sharing contract and the
/// status line. Same rationale as [`bug_pin_tests`]: none of this ran in any gate before the move.
#[path = "fold_tests.rs"]
#[cfg(test)]
mod fold_tests;

/// The DIRECT-BAR fold (split-plane B2's kline follow-up): the third mode's kline charts render
/// from the venue-fed [`DirectBarStore`], never from `snap.bars` — the render-source uniqueness
/// family, driven through the REAL fold with BOTH inputs populated. Plus the history seam (the
/// backend tail as a one-time seed) and the fold's cadence/self-healing contracts.
#[path = "direct_bar_tests.rs"]
#[cfg(test)]
mod direct_bar_tests;
