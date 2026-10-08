//! `store_bars` — **a chart asks the BACKEND'S OWN STORE for the interval it was given.**
//!
//! # The defect this module exists to close
//!
//! > *"5m/15m/1h/1s charts can never paint — WHAT??? YOU CANT GET 5MIN OR 1H DATA FROM BINANCE OR
//! > BYBIT??"*
//!
//! Nothing here is a venue limitation. Binance and Bybit serve every one of those intervals, this
//! tree has working collectors for them, and the deployed datahub's store holds what those
//! collectors wrote. The desktop simply had no way to ASK for them.
//!
//! Every bar the desktop could draw arrived over the node protocol as
//! [`observe_bridge::wire_to_core`](crate::backend::observe_bridge::wire_to_core)'s rebuild of the wire
//! snapshot's bar tails — i.e. the daemon's own core bar cache, which holds whatever its mounted
//! STRATEGIES hold. the CI box trades `1m`. So `1m` was the only interval that could paint, on any
//! symbol, and every other entry in the interval menu produced a permanently empty chart.
//!
//! The hist plane was already there and already reachable: `LoadBars` is a `VerbScope::Read`
//! verb the datahub has always served, the desktop already resolves the datahub address for the
//! Data Manager and the Studio, and [`data_sink::DirectBarStore`](crate::data::data_sink::DirectBarStore)
//! is a GUI-side bar store the fold already knows how to render. What was missing was the one call
//! joining them — this module.
//!
//! # The three rules, and why each is narrow
//!
//! [`plan_store_reads`] is the whole decision, and it is deliberately the SMALLEST claim that
//! fixes the defect:
//!
//! 1. **Only a KLINE interval.** Tick and volume charts fold from the local trade tape; the store
//!    has no such series and a read would be a wasted round trip.
//! 2. **Only a chart holding NO bars.** A painting chart is never disturbed, exactly as
//!    [`series_follow::plan_follow`](crate::ui::series_follow::plan_follow) never retargets one.
//! 3. **Only a series the backend does NOT publish.** A published series folds live from
//!    `snap.bars` and must keep doing so —
//!    [`split_plane::series_render_source`](crate::backend::split_plane::series_render_source) says the same
//!    thing from the render side under
//!    [`BarPlane::BackendStore`](crate::backend::split_plane::BarPlane::BackendStore), so the fetch rule and
//!    the render rule are the same rule read from two ends.
//!
//! …plus the ONCE rule, which is the caller's `asked` set rather than a fact about the data: a
//! chart with no bars stays a chart with no bars for as long as the read is in flight (and forever
//! if the store holds nothing), so without it rules 1–3 would re-plan the same read every frame.
//! The caller inserts the key BEFORE spawning, which is what makes "once" hold across the flight
//! rather than only across the return.
//!
//! # What this module deliberately does NOT do
//!
//! It never asks the backend to FETCH from a venue. That is the datahub's `backfill` verb, its
//! `backfill-serve` feature and [`backfill_route`](crate::data::backfill_route) — a write into the
//! server's store, gated separately. This is a pure READ of what the store already holds: against
//! a store with no `5m` rows it returns nothing and the chart's badge says so, which is an honest
//! answer and not this module's problem to solve.
//!
//! # ⚠ The one accepted residual: the frames before the first snapshot
//!
//! Rule 3 reads `published`, which the shell refreshes only when the backend's snapshot `seq`
//! advances. On the first frames of a session — before any snapshot has landed, or while a backend
//! is still dialling — that list is EMPTY, so a chart on an interval the backend is about to
//! publish (the startup default's `1m`) is asked for once before anyone knows better.
//!
//! It is a wasted round trip and nothing worse, which is why it is accepted rather than guarded:
//! the seed lands in the store, the backend's first snapshot then makes that series `published`,
//! and [`split_plane::series_render_source`](crate::backend::split_plane::series_render_source) hands the
//! key back to the snapshot fold for good — so the chart paints the daemon's live tail exactly as
//! it did before this module existed, and the seeded rows sit unread until the next backend switch
//! empties the store. ⚠ They are DISCARDED rather than merged: `ChartState::sync` rebuilds from
//! whichever list it is handed, so the store's deeper history does NOT survive under the live tail.
//! Deepening a published series' history is the backfill plane's job, not this one's.
//!
//! The alternative — refusing to read until a snapshot has arrived — would silently disable this
//! whole path against a backend that publishes nothing at all, which is precisely the case it
//! exists to serve.

use std::collections::HashSet;

use crate::data::data_sink::DirectBarStore;
use crate::ui::series_follow::{ChartTarget, PublishedSeries};
use crate::ui::workspace;
use vike_data::HistStore;
use vike_orderflow::tickvol::BarKind;

/// How many bars back one store read asks for.
///
/// Bounded rather than [`vike_data::TsRange::all`] because the store is the SERVER's and may hold
/// years: an unbounded `1m` read would move millions of rows over the wire to paint a screen that
/// shows a few hundred. Comfortably above any chart's visible width, so scrolling left has history
/// to find, and deliberately BELOW
/// [`data_sink::DIRECT_BAR_CLOSED_CAP`](crate::data::data_sink::DIRECT_BAR_CLOSED_CAP) so a full answer
/// is never trimmed on the way in — a seed that lost its oldest rows to the cap would read as a
/// store that holds less than it does.
pub const STORE_READ_BARS: i64 = 3_000;

/// One `(venue, symbol, interval)` a chart wants out of the backend's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreBarRequest {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// **What KIND of instrument `symbol` names**, when the window that wants it knows — 0061
    /// Phase 2's second drop seam, carried from
    /// [`ChartTarget::class`](crate::ui::series_follow::ChartTarget::class).
    ///
    /// ⚠ **Nothing in THIS module reads it, and that is the phase rather than an oversight.**
    /// [`read_into_store`] cannot consume it: `HistStore::load_bars` takes
    /// `(venue, symbol, interval, range)` and 0061's store-key verdict keeps the key the SYMBOL, so
    /// there is no type segment for a class to select. Its only destination is the seed request's
    /// optional class ([`crate::data::chart_seed`] → `Request::SeedSeries`), which is Phase 3.
    pub class: Option<vike_model::AssetClass>,
}

impl StoreBarRequest {
    /// The GUI-side chart/feed key — the SAME builder
    /// [`workspace::series_key`](crate::ui::workspace::series_key) gives `WinState::key`, `spawned`
    /// and `charts`, so the caller's `asked` set is keyed identically to everything else and a
    /// membership test is one string comparison rather than a re-derivation.
    ///
    /// ⚠ **[`Self::class`] is deliberately NOT part of it, and widening it would break the ONCE
    /// rule rather than refine it.** This is `series_key`, the key `App::store_asked`, `spawned`,
    /// `charts` and [`plan_store_reads`]' own `planned` set are all keyed on; a key carrying a
    /// class would no longer match the ledger those hold, and a chart would be asked once per
    /// frame again. It is also 0061's store-key verdict read from the GUI side: the symbol IS the
    /// series, so two spellings that differ only by a claim name ONE store partition, and giving
    /// them two keys here would promise a separation the store does not have.
    ///
    /// **The consequence, stated because it is a real behaviour question and not an implementation
    /// detail:** two chart windows on the same `(venue, symbol, interval)` with DIFFERENT classes
    /// deduplicate to one request, and the survivor is the first in `targets` order. FIRST-WINS
    /// rather than a refusal, because the alternative — refusing both — would blank two charts over
    /// a disagreement the store cannot express either way, and because the seed door refuses a
    /// claim it cannot honour on that spelling anyway
    /// (`crates/vike-datahub/src/server/seed_series.rs`'s `seed_series_verb`), so the survivor is checked
    /// there rather than trusted here. `two_windows_on_one_series_with_different_classes_ask_once`
    /// pins it.
    pub fn key(&self) -> String {
        workspace::series_key(&self.venue, &self.symbol, &self.interval)
    }
}

/// The inclusive range one read asks for: the last [`STORE_READ_BARS`] bars ending `now_ms`.
///
/// `None` for an interval [`vike_model::time::interval_ms`] cannot parse — and that refusal is the
/// point rather than a gap. An unparseable interval has no bar width, so there is no bounded window
/// to ask for, and answering [`vike_data::TsRange::all`] instead would hand a typo an unbounded
/// scan of somebody's production store. A store holding no such series answers empty either way.
pub fn read_range(interval: &str, now_ms: i64) -> Option<vike_data::TsRange> {
    let step = vike_model::time::interval_ms(interval).filter(|&ms| ms > 0)?;
    Some(vike_data::TsRange::of(
        now_ms.saturating_sub(step.saturating_mul(STORE_READ_BARS)),
        now_ms,
    ))
}

/// **THE RULE**, as a pure function — see the module doc for each clause's argument.
///
/// `targets` is [`series_follow::chart_targets`](crate::ui::series_follow::chart_targets)' output (the
/// open chart windows, lifted off the live list with `has_bars` already resolved against `charts`);
/// `published` is what the backend's live snapshot carries
/// ([`series_follow::published_series`](crate::ui::series_follow::published_series)); `asked` is the
/// caller's set of keys already requested this session.
///
/// Deduplicated by key, so two windows on the same series produce ONE read, and in `targets` order
/// so a plan is a function of the window list rather than of hash iteration.
pub fn plan_store_reads(
    targets: &[ChartTarget],
    published: &[PublishedSeries],
    asked: &HashSet<String>,
) -> Vec<StoreBarRequest> {
    let mut out: Vec<StoreBarRequest> = Vec::new();
    let mut planned: HashSet<String> = HashSet::new();
    for t in targets {
        if !matches!(BarKind::parse(&t.interval), BarKind::Kline(_)) {
            continue; // tick/volume charts fold from the local tape — the store has no such series
        }
        if t.has_bars {
            continue; // already painting: never disturbed, same rule as adoption's
        }
        if published
            .iter()
            .any(|p| p.venue == t.venue && p.symbol == t.symbol && p.interval == t.interval)
        {
            continue; // the backend streams this one; it folds live from `snap.bars`
        }
        if read_range(&t.interval, 0).is_none() {
            continue; // no parsable bar width ⇒ no bounded window to ask for (see `read_range`)
        }
        let key = workspace::series_key(&t.venue, &t.symbol, &t.interval);
        if asked.contains(&key) || !planned.insert(key) {
            continue; // asked once per session, and once per plan
        }
        out.push(StoreBarRequest {
            venue: t.venue.clone(),
            symbol: t.symbol.clone(),
            interval: t.interval.clone(),
            // Carried, not read — see the field's doc. A COPY: `AssetClass` is `Copy`.
            class: t.class,
        });
    }
    out
}

/// What one batch of store reads found — [`read_into_store`]'s answer.
///
/// ⚠ **`empty` is the whole reason this is a struct rather than the `usize` it used to be.** "The
/// store holds no rows for this series" is the ONE outcome a caller can act on: it is the input to
/// the chart-gap seed ([`crate::data::chart_seed`]), which asks the SERVER to fetch what its store does
/// not have. An error is deliberately NOT in this list — a read that failed says nothing about
/// whether the series exists, and asking a server to fetch on the strength of a transport failure
/// would turn one bad moment into venue traffic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreReadOutcome {
    /// Series that landed bars (the old `usize` return).
    pub landed: usize,
    /// Series the store answered EMPTY for, in request order. The seed plan's input.
    pub empty: Vec<StoreBarRequest>,
}

/// Perform `reqs` against `store` and land each answer in `bars` — the BODY of the background
/// read, exposed separately from [`spawn_store_seed`] so it is driven synchronously by tests
/// against a real [`vike_data::HistStore`] double instead of being reachable only through a thread.
///
/// Returns a [`StoreReadOutcome`]: how many series landed, and which answered EMPTY. Per-request
/// failure is logged and SKIPPED rather than aborting the batch: one unreadable series must not
/// cost the others their history, and the caller has already recorded every key as asked, so
/// nothing retries either way.
///
/// ⚠ An EMPTY answer lands NOTHING. [`DirectBarStore::seed`] on an empty vec would create an empty
/// series, bump the generation and leave the chart just as blank — while making
/// [`series_follow::chart_feed`](crate::ui::series_follow::chart_feed) and the empty-plot hint claim a
/// series exists. "The store has no 5m rows for this symbol" is a real answer and the operator is
/// entitled to see it said.
pub fn read_into_store(
    reqs: &[StoreBarRequest],
    store: &dyn HistStore,
    bars: &DirectBarStore,
    now_ms: i64,
) -> StoreReadOutcome {
    let mut out = StoreReadOutcome::default();
    for r in reqs {
        let Some(range) = read_range(&r.interval, now_ms) else { continue };
        match store.load_bars(&r.venue, &r.symbol, &r.interval, range) {
            Ok(rows) if rows.is_empty() => {
                tracing::info!(
                    series = %r.key(),
                    "the backend's store holds no bars for this series (the chart stays empty                      unless the server's chart-seed lane is armed)"
                );
                out.empty.push(r.clone());
            }
            Ok(rows) => {
                tracing::info!(
                    series = %r.key(),
                    bars = rows.len(),
                    "seeded a chart from the backend's store"
                );
                bars.seed(&r.venue, &r.symbol, &r.interval, rows);
                out.landed += 1;
            }
            // ⚠ NOT collected into `empty`: see `StoreReadOutcome::empty`. A failed read is not
            // evidence that a series is missing, and treating it as such would turn a transport
            // blip into a venue fetch.
            Err(e) => tracing::warn!(series = %r.key(), "store read failed: {e}"),
        }
    }
    out
}

/// [`read_into_store`] on its own thread, waking the GUI once at the end.
///
/// ONE thread for the whole batch rather than one per request: `RemoteHistStore` opens a fresh
/// connection per read anyway, so the parallelism would buy latency at the cost of N sockets
/// against the operator's server for a screen that is about to show one of them.
///
/// The wake is a single repaint at the end — the fold that consumes the store is driven by
/// [`DirectBarStore::generation`], which the seeds already bumped, so one frame is enough for
/// every series in the batch.
/// ⚠ **`seed` is the GAP arm and it runs on THIS SAME THREAD, after the read.** When the store
/// answers EMPTY for a series and the caller supplied a dial, this asks the SERVER to fetch that
/// series ([`crate::data::chart_seed`]) and then RE-READS the ones it reported rows for, so the chart
/// paints from one wake rather than from a second frame's plan.
///
/// One thread, not two, and the ordering is the reason: the seed's input is the read's output, and
/// the re-read's input is the seed's. Spawning either separately would buy nothing and would need a
/// second ledger to stay idempotent. `None` is byte-identical to the behaviour before the gap arm
/// existed — which is what a caller with no resolved datahub address passes.
pub fn spawn_store_seed<S, W>(
    reqs: Vec<StoreBarRequest>,
    store: S,
    bars: std::sync::Arc<DirectBarStore>,
    now_ms: i64,
    seed: Option<crate::data::chart_seed::SeedDial>,
    wake: W,
) -> std::thread::JoinHandle<()>
where
    S: HistStore + Send + 'static,
    W: Fn() + Send + 'static,
{
    std::thread::spawn(move || {
        let outcome = read_into_store(&reqs, &store, &bars, now_ms);
        if let Some(dial) = seed
            && !outcome.empty.is_empty()
        {
            let refetched = dial.run(&outcome.empty);
            if !refetched.is_empty() {
                // Only the series the server said it now holds — a second read for one it fetched
                // nothing for would be a round trip whose answer is already known.
                read_into_store(&refetched, &store, &bars, now_ms);
            }
        }
        wake();
    })
}

#[path = "store_bars_tests.rs"]
#[cfg(test)]
mod store_bars_tests;
