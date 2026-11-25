//! `stored` — **a chart asks the BACKEND'S OWN STORE for the interval it was given.**
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
//! Every bar the desktop could draw arrived over the hub protocol as
//! [`observe_decode::wire_to_core`](vike_tradehub_client::observe_decode::wire_to_core)'s rebuild of the wire
//! snapshot's bar tails — i.e. the daemon's own core bar cache, which holds whatever its mounted
//! STRATEGIES hold. the CI box trades `1m`. So `1m` was the only interval that could paint, on any
//! symbol, and every other entry in the interval menu produced a permanently empty chart.
//!
//! The hist plane was already there and already reachable: `LoadBars` is a `VerbScope::Read`
//! verb the datahub has always served, the desktop already resolves the datahub address for the
//! Data Manager and the Studio, and [`data_sink::StoredBarStore`](crate::data::data_sink::StoredBarStore)
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
//! 2. **Only what the chart SHOWS.** An empty chart asks for its newest view's worth of bars; a
//!    painting one asks again only when its view moved or zoomed past what it holds, for the window it
//!    now shows at the bar width that view needs (`history`'s module doc: the budget, the debounce,
//!    the resample, the cache). Against an older datahub (no `bar_resample`) the rule is the one it
//!    replaced: only a chart holding NO bars, and only the last `history_bars` bars of its interval.
//! 3. **A series the backend publishes is asked only for what lies BEFORE its live tail.** It folds
//!    live from `snapshot.bars` and must keep doing so —
//!    `split_plane::series_render_source` says the same
//!    thing from the render side under
//!    [`BackendBarPlane::BackendStore`](crate::backend::split_plane::BackendBarPlane::BackendStore), so the fetch rule and
//!    the render rule are the same rule read from two ends. The node ships only its newest closes
//!    (`crates/vike-tradehub/src/publish.rs`'s `CHART_BARS_CAP`), so a view panned left of the
//!    oldest asks the store for the bars of its own interval in between
//!    (`ChartHistory::plan_under_live_tail`), and the snapshot fold lays the tail over them
//!    ([`StoredBarStore::under_live_tail`]). Until 2026-11-19 it was never asked at all, and
//!    panning a published chart left loaded nothing.
//!
//! …plus the ONCE rule, which is [`ChartHistory`]'s rather than a fact about the data: a chart with no
//! bars stays a chart with no bars for as long as the read is in flight (and forever if the store holds
//! nothing), so without it rules 1–3 would re-plan the same read every frame. [`plan_store_reads`]
//! records the key (in flight, or asked once) BEFORE the caller spawns, which is what makes "once" hold
//! across the flight rather than only across the return.
//!
//! # What this module deliberately does NOT do
//!
//! It never asks the backend to FETCH from a venue. That is the datahub's `backfill` verb, its
//! `backfill-serve` feature and [`backfill`](crate::data::backfill) — a write into the
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
//! and `split_plane::series_render_source` hands the
//! key back to the snapshot fold for good — so the chart paints the daemon's live tail exactly as
//! it did before this module existed, and the seeded rows sit unread until the next backend switch
//! empties the store. ⚠ They are DISCARDED rather than merged: `ChartState::sync` rebuilds from
//! whichever list it is handed, and only a read under the live tail (rule 3) marks a series to be
//! laid under it.
//!
//! The alternative — refusing to read until a snapshot has arrived — would silently disable this
//! whole path against a backend that publishes nothing at all, which is precisely the case it
//! exists to serve.

use std::collections::HashSet;

use crate::data::chart_key;
use crate::data::data_sink::StoredBarStore;
use crate::ui::series_follow::{ChartSeriesFollowTarget, PublishedSeries};
use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_datahub_client::HistoryStoreDatahub;
use vike_marketdata::Bar;
use vike_orderflow::tickvol::BarKind;

pub mod chart_seed;
mod history;

use history::ChartHistoryPlan;
pub use history::{
    CACHE_BARS_CEILING, ChartHistory, ChartHistoryCache, ChartHistoryWindow, MAX_VIEW_CANDLES,
    VIEW_DEBOUNCE_MS, VIEW_SLACK_BARS,
};

// ⚠ **How many bars back the read an OLDER datahub gets asks for is the caller's `history_bars`
// argument** (the `preferences.chart_history_bars` row, 3,000 unless a row says otherwise), not a
// constant here; it was `STORE_READ_BARS` before the row existed. A datahub that resamples is asked
// by the view instead (`history`), and the row does not apply to it.
//
// Bounded rather than [`vike_data::TimestampRange::all`] because the store is the SERVER's and may hold
// years: an unbounded `1m` read would move millions of rows over the wire to paint a screen that
// shows a few hundred. The row's floor is comfortably above any chart's visible width, so scrolling
// left has history to find, and its ceiling is held BELOW
// [`data_sink::STORED_BAR_CLOSED_CAP`](crate::data::data_sink::STORED_BAR_CLOSED_CAP) by a `const`
// assertion in `data_sink`, so a full answer is never trimmed on the way in — a seed that lost its
// oldest rows to the cap would read as a store that holds less than it does.

/// One `(venue, symbol, interval)` a chart wants out of the backend's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBarRequest {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// **What KIND of instrument `symbol` names**, when the window that wants it knows — 0061
    /// Phase 2's second drop seam, carried from
    /// `ChartSeriesFollowTarget::class`.
    ///
    /// ⚠ **Nothing in THIS module reads it, and that is the phase rather than an oversight.**
    /// [`read_into_store`] cannot consume it: `HistoryStore::load_bars` takes
    /// `(venue, symbol, interval, range)` and 0061's store-key verdict keeps the key the SYMBOL, so
    /// there is no type segment for a class to select. Its only destination is the seed request's
    /// optional class ([`crate::data::stored::chart_seed`] → `Request::SeedSeries`), which is Phase 3.
    pub class: Option<vike_model::AssetClass>,
    /// The window this read fills, planned from the chart's view; `None` is the read an older datahub
    /// gets (the last `history_bars` bars of `interval`, [`read_range`]).
    pub view: Option<StoredBarView>,
}

/// The window a view-planned read fills, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBarView {
    /// What the chart will hold: every bar of the window's width that opens in it.
    pub window: ChartHistoryWindow,
    /// The interval the datahub folds `interval` into (`Request::LoadBars`' `resample_to`); `None` for the
    /// chart's own interval.
    pub resample_to: Option<&'static str>,
    /// The pieces of the window the session's cache does not hold, the only ones read: empty is a cache
    /// hit, which sends nothing.
    pub missing: Vec<(i64, i64)>,
    /// The chart held no bars: an empty answer is then the chart-seed gap arm's input
    /// ([`StoredReadOutcome::empty`]), and is nothing more for a chart that paints.
    pub opening: bool,
    /// The OLDER history of a series the node publishes, read at its own interval on any datahub
    /// and laid under the node's tail ([`StoredBarStore::seed_under_live_tail`]).
    pub under_live_tail: bool,
}

/// What a chart's history read needs of the backend's store beyond [`HistoryStore`]: the datahub's
/// RESAMPLED read (`Request::LoadBars`' `resample_to`), which the trait has no verb for. A plain store reads
/// as an older datahub does: it does not resample.
pub trait ChartBarSource: HistoryStore {
    /// Whether the store resamples: `None` when it could not be asked (the read that follows says why).
    fn serves_resample(&self) -> Option<bool> {
        Some(false)
    }

    /// The bars of `interval` over `range` folded into bars of `to`; `Ok(None)` when the store does not
    /// resample, nothing sent.
    fn load_bars_resampled(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        to: &str,
    ) -> Result<Option<Vec<Bar>>, HistoryStoreError> {
        let _ = (venue, symbol, interval, range, to);
        Ok(None)
    }
}

impl ChartBarSource for dyn HistoryStore + '_ {}

impl ChartBarSource for HistoryStoreDatahub {
    fn serves_resample(&self) -> Option<bool> {
        let features = self.advertised_features().ok()?;
        Some(features.iter().any(|f| f == vike_datahub_client::proto::FEATURE_BAR_RESAMPLE))
    }

    fn load_bars_resampled(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        to: &str,
    ) -> Result<Option<Vec<Bar>>, HistoryStoreError> {
        HistoryStoreDatahub::load_bars_resampled(self, venue, symbol, interval, range, to)
    }
}

impl StoredBarRequest {
    /// The GUI-side chart/feed key — the SAME builder
    /// [`chart_key::series_key`] gives `WindowState::key`, `spawned`
    /// and `charts`, so the caller's `asked` set is keyed identically to everything else and a
    /// membership test is one string comparison rather than a re-derivation.
    ///
    /// ⚠ **[`Self::class`] is deliberately NOT part of it, and widening it would break the ONCE
    /// rule rather than refine it.** This is `series_key`, the key `App::chart_history`, `spawned`,
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
        chart_key::series_key(&self.venue, &self.symbol, &self.interval)
    }
}

/// One bar's width in milliseconds, or `None` for an interval with none
/// ([`vike_marketdata::interval::bar_step_ms`]). The one parse [`read_range`] and
/// [`plan_store_reads`] both ask, so a request the plan lets through is one the read can bound.
fn bar_width_ms(interval: &str) -> Option<i64> {
    vike_marketdata::interval::bar_step_ms(interval)
}

/// The inclusive range one read asks for: the last `history_bars` bars ending `now_ms` —
/// `preferences.chart_history_bars`, the caller's row.
///
/// `None` for an interval [`vike_marketdata::interval::interval_ms`] cannot parse — and that refusal is the
/// point rather than a gap. An unparseable interval has no bar width, so there is no bounded window
/// to ask for, and answering [`vike_data::TimestampRange::all`] instead would hand a typo an unbounded
/// scan of somebody's production store. A store holding no such series answers empty either way.
pub fn read_range(
    interval: &str,
    now_ms: i64,
    history_bars: i64,
) -> Option<vike_data::TimestampRange> {
    let step = bar_width_ms(interval)?;
    Some(vike_data::TimestampRange::of(
        now_ms.saturating_sub(step.saturating_mul(history_bars)),
        now_ms,
    ))
}

/// **THE RULE** — see the module doc for each clause's argument.
///
/// `targets` is [`series_follow::chart_targets`](crate::ui::series_follow::chart_targets)' output (the
/// open chart windows, lifted off the live list with `has_bars` already resolved against `charts`);
/// `published` is what the backend's live snapshot carries
/// ([`series_follow::published_series`](crate::ui::series_follow::published_series)); `history` is the
/// session's ledger: what each window shows ([`ChartHistory::observe_view`]), what each chart was given,
/// what is in flight and the cache. Every key planned is recorded there before this returns.
///
/// Deduplicated by key, so two windows on the same series produce ONE read (the first window's view),
/// and in `targets` order so a plan is a function of the window list rather than of hash iteration.
pub fn plan_store_reads(
    targets: &[ChartSeriesFollowTarget],
    published: &[PublishedSeries],
    history: &mut ChartHistory,
    now_ms: i64,
) -> Vec<StoredBarRequest> {
    let mut out: Vec<StoredBarRequest> = Vec::new();
    let mut planned: HashSet<String> = HashSet::new();
    for t in targets {
        if t.venue.is_empty() || t.symbol.is_empty() {
            continue; // an EMPTY chart (nothing picked yet) names no series to read
        }
        if !matches!(BarKind::parse(&t.interval), BarKind::Kline(_)) {
            continue; // tick/volume charts fold from the local tape — the store has no such series
        }
        // The backend streams this one: it folds live from `snapshot.bars`, and the store is asked
        // only for what a view panned left of that tail shows.
        let live = published
            .iter()
            .any(|p| p.venue == t.venue && p.symbol == t.symbol && p.interval == t.interval);
        let Some(native) = bar_width_ms(&t.interval) else {
            continue; // no parsable bar width ⇒ no bounded window to ask for (see `read_range`)
        };
        let key = chart_key::series_key(&t.venue, &t.symbol, &t.interval);
        if !planned.insert(key.clone()) {
            continue; // once per plan: the first window on a series speaks for it
        }
        let plan = match (live, t.first_open) {
            (true, Some(first)) => history.plan_under_live_tail(t.id, &key, native, first, now_ms),
            (true, None) => None, // the tail has not painted yet
            (false, _) => history.plan(t.id, &key, native, t.has_bars, now_ms),
        };
        let view = match plan {
            None => continue, // nothing due: in flight, held, asked once, or a view not yet still
            Some(ChartHistoryPlan::Classic) => None,
            Some(ChartHistoryPlan::View { window, resample_to, missing }) => Some(StoredBarView {
                window,
                resample_to,
                missing,
                opening: !t.has_bars,
                under_live_tail: live,
            }),
        };
        out.push(StoredBarRequest {
            venue: t.venue.clone(),
            symbol: t.symbol.clone(),
            interval: t.interval.clone(),
            // Carried, not read — see the field's doc. A COPY: `AssetClass` is `Copy`.
            class: t.class,
            view,
        });
    }
    out
}

impl ChartHistory {
    /// [`plan_store_reads`] for this frame's chart windows, now: the shell's one call, so the wiring of the
    /// targets stays here (`series_follow::chart_targets`).
    pub fn plan_reads(
        &mut self,
        windows: &[crate::ui::window::WindowState],
        charts: &std::collections::HashMap<String, vike_chart::chart::state::ChartState>,
        published: &[PublishedSeries],
    ) -> Vec<StoredBarRequest> {
        let targets = crate::ui::series_follow::chart_targets(windows, charts);
        plan_store_reads(&targets, published, self, vike_model::now_ms())
    }
}

/// What one batch of store reads found — [`read_into_store`]'s answer.
///
/// ⚠ **`empty` is the whole reason this is a struct rather than the `usize` it used to be.** "The
/// store holds no rows for this series" is the ONE outcome a caller can act on: it is the input to
/// the chart-gap seed ([`crate::data::stored::chart_seed`]), which asks the SERVER to fetch what its store does
/// not have. An error is deliberately NOT in this list — a read that failed says nothing about
/// whether the series exists, and asking a server to fetch on the strength of a transport failure
/// would turn one bad moment into venue traffic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredReadOutcome {
    /// Series that landed bars (the old `usize` return).
    pub landed: usize,
    /// Series the store answered EMPTY for, in request order. The seed plan's input.
    pub empty: Vec<StoredBarRequest>,
    /// One sentence per series whose read FAILED, for the chart that stays blank because of it.
    pub failed: Vec<String>,
}

/// Perform `reqs` against `store` and land each answer in `bars` — the BODY of the background
/// read, exposed separately from [`spawn_store_seed`] so it is driven synchronously by tests
/// against a real [`vike_data::HistoryStore`] double instead of being reachable only through a thread.
///
/// `history_bars` is how many bars back each read asks for (see [`read_range`]) — the caller's
/// `preferences.chart_history_bars` row, read by the BINARY: this is a library, so the depth
/// arrives as a parameter and is not a constant here.
///
/// Returns a [`StoredReadOutcome`]: how many series landed, and which answered EMPTY. Per-request
/// failure is logged and SKIPPED rather than aborting the batch: one unreadable series must not
/// cost the others their history, and the caller has already recorded every key as asked, so
/// nothing retries either way.
///
/// ⚠ An EMPTY answer lands NOTHING. [`StoredBarStore::seed`] on an empty vec would create an empty
/// series, bump the generation and leave the chart just as blank — while making
/// [`series_follow::chart_feed`](crate::ui::series_follow::chart_feed) and the empty-plot hint claim a
/// series exists. "The store has no 5m rows for this symbol" is a real answer and the operator is
/// entitled to see it said.
///
/// A request with a [`StoredBarView`] reads only its `missing` pieces (resampled by the datahub when it
/// names `resample_to`), keeps them in `cache`, and lands the cache's bars of its window. A store that does
/// not resample ([`ChartBarSource::serves_resample`], asked once per session) is read the older way
/// instead, as is every request after a resampled read was refused for want of the feature.
pub fn read_into_store<S: ChartBarSource + ?Sized>(
    reqs: &[StoredBarRequest],
    store: &S,
    bars: &StoredBarStore,
    cache: &ChartHistoryCache,
    now_ms: i64,
    history_bars: i64,
) -> StoredReadOutcome {
    let mut out = StoredReadOutcome::default();
    let unknown = cache.lock().resamples.is_none();
    if unknown && reqs.iter().any(|r| r.view.is_some()) {
        let answer = store.serves_resample();
        cache.lock().resamples = answer;
    }
    for r in reqs {
        // Each lock in a statement of its own: no guard may live into a read, which locks again.
        let older = |cache: &ChartHistoryCache| cache.lock().resamples == Some(false);
        if let Some(view) = &r.view
            && (view.under_live_tail || !older(cache))
        {
            let read = read_view(r, view, store, bars, cache, now_ms);
            cache.lock().in_flight.remove(&r.key());
            match read {
                ViewRead::Landed => out.landed += 1,
                ViewRead::Empty if view.opening => out.empty.push(r.clone()),
                ViewRead::Empty if view.under_live_tail => tracing::info!(
                    series = %r.key(),
                    "the backend's store holds no bars older than this chart's live tail"
                ),
                ViewRead::Empty => {}
                ViewRead::Failed(why) => out.failed.push(why),
                ViewRead::NotResampled => {}
            }
            if view.under_live_tail || !older(cache) {
                continue;
            }
        }
        // The read an older datahub gets, once per session.
        let mut state = cache.lock();
        state.in_flight.remove(&r.key());
        state.asked.insert(r.key());
        drop(state);
        let Some(range) = read_range(&r.interval, now_ms, history_bars) else { continue };
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
            // ⚠ NOT collected into `empty`: see `StoredReadOutcome::empty`. A failed read is not
            // evidence that a series is missing, and treating it as such would turn a transport
            // blip into a venue fetch.
            Err(e) => {
                tracing::warn!(series = %r.key(), "store read failed: {e}");
                out.failed.push(read_failed(&r.key(), &e));
            }
        }
    }
    out
}

fn read_failed(key: &str, e: &HistoryStoreError) -> String {
    format!(
        "The history of {key} could not be read from the backend's store ({e}), so this chart is \
         empty because the read failed, not because the store holds nothing."
    )
}

/// What one view-planned read did.
enum ViewRead {
    Landed,
    Empty,
    Failed(String),
    /// The store does not resample after all (an older datahub): the caller reads the older way.
    NotResampled,
}

/// Read `view`'s missing pieces into `cache`, then land the cache's bars of its window in `bars` and mark
/// the display for the chart's re-frame. An empty window lands nothing: the chart keeps what it shows.
fn read_view<S: ChartBarSource + ?Sized>(
    r: &StoredBarRequest,
    view: &StoredBarView,
    store: &S,
    bars: &StoredBarStore,
    cache: &ChartHistoryCache,
    now_ms: i64,
) -> ViewRead {
    let key = r.key();
    for &(lo, hi) in &view.missing {
        let range = TimestampRange::of(lo, hi);
        let rows = match view.resample_to {
            None => store.load_bars(&r.venue, &r.symbol, &r.interval, range).map(Some),
            Some(to) => store.load_bars_resampled(&r.venue, &r.symbol, &r.interval, range, to),
        };
        match rows {
            Ok(Some(rows)) => cache.lock().hold(&key, view.window.step_ms, (lo, hi), rows, now_ms),
            Ok(None) => {
                cache.lock().resamples = Some(false);
                return ViewRead::NotResampled;
            }
            Err(e) => {
                tracing::warn!(series = %key, "store read failed: {e}");
                return ViewRead::Failed(read_failed(&key, &e));
            }
        }
    }
    let shown = cache.lock().bars_in(&key, view.window);
    let (Some(first), Some(last)) = (shown.first(), shown.last()) else { return ViewRead::Empty };
    let landed = (first.timestamp, last.timestamp, shown.len());
    tracing::info!(
        series = %key,
        bars = shown.len(),
        width_ms = view.window.step_ms,
        asked = view.missing.len(),
        "a chart's view landed from the backend's store"
    );
    if view.under_live_tail {
        // The chart re-frames by itself on the jump in its bar count (`vike_chart`'s seed reset).
        bars.seed_under_live_tail(&r.venue, &r.symbol, &r.interval, shown);
        return ViewRead::Landed;
    }
    bars.seed(&r.venue, &r.symbol, &r.interval, shown);
    cache.lock().landed.insert(key, landed);
    ViewRead::Landed
}

/// [`read_into_store`] on its own thread, waking the GUI once at the end.
///
/// ONE thread for the whole batch rather than one per request: `HistoryStoreDatahub` opens a fresh
/// connection per read anyway, so the parallelism would buy latency at the cost of N sockets
/// against the operator's server for a screen that is about to show one of them.
///
/// The wake is a single repaint at the end — the fold that consumes the store is driven by
/// [`StoredBarStore::generation`], which the seeds already bumped, so one frame is enough for
/// every series in the batch.
/// ⚠ **`seed` is the GAP arm and it runs on THIS SAME THREAD, after the read.** When the store
/// answers EMPTY for a series and the caller supplied a dial, this asks the SERVER to fetch that
/// series ([`crate::data::stored::chart_seed`]) and then RE-READS the ones it reported rows for, so the chart
/// paints from one wake rather than from a second frame's plan.
///
/// One thread, not two, and the ordering is the reason: the seed's input is the read's output, and
/// the re-read's input is the seed's. Spawning either separately would buy nothing and would need a
/// second ledger to stay idempotent. `None` is byte-identical to the behaviour before the gap arm
/// existed — which is what a caller with no resolved datahub address passes.
pub fn spawn_store_seed<S, W>(
    reqs: Vec<StoredBarRequest>,
    store: S,
    bars: std::sync::Arc<StoredBarStore>,
    cache: std::sync::Arc<ChartHistoryCache>,
    now_ms: i64,
    history_bars: i64,
    seed: Option<crate::data::stored::chart_seed::ChartSeedDial>,
    wake: W,
) -> std::thread::JoinHandle<()>
where
    S: ChartBarSource + Send + 'static,
    W: Fn() + Send + 'static,
{
    std::thread::Builder::new()
        .name("ui-store-seed".into())
        .spawn(move || {
            let outcome = read_into_store(&reqs, &store, &bars, &cache, now_ms, history_bars);
            let mut failed = outcome.failed;
            if let Some(dial) = seed {
                if !outcome.empty.is_empty() {
                    let refetched = dial.run(&outcome.empty);
                    if !refetched.is_empty() {
                        // Only the series the server said it now holds — a second read for one it
                        // fetched nothing for would be a round trip whose answer is already known.
                        let reread = read_into_store(
                            &refetched,
                            &store,
                            &bars,
                            &cache,
                            now_ms,
                            history_bars,
                        );
                        failed.extend(reread.failed);
                    }
                }
                // After the gap arm, whose note says nothing about these. Nothing retries a failed
                // read (the caller recorded the key as asked), so this is all the operator gets.
                if !failed.is_empty()
                    && let Ok(mut slot) = dial.note.lock()
                {
                    *slot = Some(failed.join(" "));
                }
            }
            wake();
        })
        .expect("spawn ui-store-seed thread")
}

#[path = "stored_tests.rs"]
#[cfg(test)]
mod stored_tests;
