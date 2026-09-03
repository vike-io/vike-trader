//! GUI render model — the chart's OHLC bars + overlays.
//!
//! Bars use an **integer bar-index x** (0, 1, 2, …) exactly like vike's
//! `ui/chart.py` (timestamps are mapped to axis labels separately). The core owns
//! the live data layer; this render model is populated per `CoreSnapshot` and this
//! GUI only renders it.
//!
//! The legacy Python→Rust JSON-lines IPC bridge (`src/feed.rs` + the `Msg` enum +
//! `ChartState::apply`) was retired when the live feed moved to the venue-adapter crates; it
//! had no live callers.

use crate::chart::{heikin_ashi, heikin_ashi_bar, ChartStyle};
use crate::transforms::{self, Kagi, PnFColumn};
use crate::tz::DisplayTz;
use serde::Deserialize;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use vike_orderflow::{FootprintBar, VolumeProfile};

/// Cache key for [`ChartState::visible_vol_max`]: `(lo, hi, cache_key)` — the visible
/// index window plus the closed-prefix identity ([`ChartState`]'s existing `cache_key`),
/// so a bar close (which changes `cache_key`) invalidates it same as a window move.
type VolCacheKey = (usize, usize, (usize, i64, i64, u32));

/// Cache key for [`ChartState::visible_profile`] (SP2, T4): `(i0, i1, footprint_len,
/// tick_size.to_bits())` — mirrors [`VolCacheKey`]'s "visible window + a data-identity proxy"
/// shape, but the footprint substrate isn't stored on `ChartState` itself (`chart::draw`
/// receives it fresh each frame via `ChartInputs::footprint`, unlike `bars`/`cache_key`), so
/// there is no timestamp pair to fold in; length is a safe proxy because footprints are
/// appended one-per-closed-bar and never mutated after `close_bar()`. The tick size's bit
/// pattern is included too (not just `(i0, i1, len)`): a DERIVED size (`of_tick_size <= 0.0`)
/// can shift tick-to-tick as the forming bar's H/L crosses a "nice-step" tier without the
/// footprint length changing at all, so the raw f64 must be part of the key or a stale bucket
/// width would serve from cache.
type ProfileCacheKey = (usize, usize, usize, u64);

/// Cache key for [`ChartState::cvd_shared`] (SP3 Task B #2 — SP2 final-review finding B; hardened
/// by SP3 TB-fix, a post-Task-B review finding, MEDIUM): `(generation, len)`, where `generation`
/// is `ChartInputs::footprint_gen` — vike-app's `OrderflowAgg::generation()`, a monotonic counter
/// bumped every time that aggregator's footprint cache is actually rebuilt. `len` (the
/// `footprints` slice's length) rides along as a belt-and-suspenders second component, same role
/// as in [`ProfileCacheKey`]. Unlike [`ProfileCacheKey`] this has no visible-range component — the
/// CVD pane plots the FULL running cumulative-delta series, not a `[i0, i1]` window — so there's
/// nothing else to fold in.
///
/// This used to be `(footprints.as_ptr() as usize, len)` — the slice's own address+length,
/// trustworthy only as long as an unchanged aggregator keeps handing out the SAME
/// `Arc<Vec<FootprintBar>>` (and therefore the same backing-buffer address) every frame, which
/// SP3 Task B #1's cache does. That guarantee has a narrow ABA gap: toggling the CVD pane off
/// stops this cache from being read at all, but `OrderflowAgg::cache` keeps rebuilding underneath
/// it (new trades keep landing — more so once backfill raises reingest frequency), freeing and
/// reallocating a `Vec<FootprintBar>` buffer on every rebuild. By the time CVD comes back on, the
/// allocator may have recycled a freed buffer's address for the CURRENT one; if the two also
/// happen to share a length, the stale `(ptr, len)` key reads as unchanged and this cache would
/// serve outdated CVD data for a frame. A monotonic generation can't be reused this way — the
/// caller only advances it when the content genuinely changed — so it closes the gap.
type CvdCacheKey = (u64, usize);

/// One OHLC bar. `t` is the bar index, NOT a unix timestamp.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Bar {
    pub t: f64, // bar index (x)
    #[serde(default)]
    pub ot: i64, // open time (ms) — for the time x-axis
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    #[serde(default)]
    pub v: f64, // base-asset volume
}

/// Perf-gate telemetry for [`ChartState::sync`]: how much rendering the last call
/// actually did. `closed_rerendered` is the count of closed bars re-rendered into
/// `bars` (0 on an unchanged-prefix forming tick, the delta on an append, the full
/// length on a structural rebuild); `forming_rerendered` is whether a forming bar
/// was rendered this call.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub struct SyncWork {
    pub closed_rerendered: usize,
    pub forming_rerendered: bool,
}

/// Perf-gate telemetry for [`ChartState::refresh_caches`]: how many CLOSED bars had
/// their `y_ext`/`hour_marks` contribution (re)computed on the last call — 0 on an
/// unchanged-key call (forming tick), the append delta on a bar close, the full
/// closed length on a structural rebuild (reload/shrink/first-bar change).
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub struct RefreshWork {
    pub recomputed: usize,
}

/// Params the six transform styles take, threaded through [`ChartState::transformed`] and
/// folded into its cache key. Only the field(s) a given style reads matter — `line_break_n` for
/// `LineBreak`, `pnf_reversal` for `PointFigure`; the others derive their box/reversal size from
/// the bars themselves (`transforms::auto_box`) and take no param. The chart call site passes the
/// same constants it always has (`line_break(bars, 3)` / `point_and_figure(bars, 3)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TransformParams {
    /// `n` for `transforms::line_break` (call site: 3).
    pub line_break_n: usize,
    /// `reversal` for `transforms::point_and_figure` (call site: 3).
    pub pnf_reversal: i32,
}

/// The [`ChartState::transformed`] cache key: the transform identity that, while unchanged, keeps
/// the CLOSED-prefix transform valid. `cache_key` is [`ChartState`]'s existing closed-prefix
/// identity `(closed_len, first ot, last ot, tz identity)` — a bar close (or a `set_tz`) bumps
/// it, invalidating the closed transform; a forming tick leaves it (only `forming_fp` moves).
#[derive(Clone, Copy, PartialEq)]
struct TransformKey {
    style: ChartStyle,
    params: TransformParams,
    cache_key: (usize, i64, i64, u32),
}

/// Bit fingerprint of the forming bar `(t, ot, o, h, l, c, v)` — compared bit-for-bit (not `==`)
/// so the cached full transform is only reused when the forming bar is byte-identical.
type FormingFp = (u64, i64, u64, u64, u64, u64, u64);

fn forming_fingerprint(b: &Bar) -> FormingFp {
    (b.t.to_bits(), b.ot, b.o.to_bits(), b.h.to_bits(), b.l.to_bits(), b.c.to_bits(), b.v.to_bits())
}

/// Cap mirroring `transforms::MAX_UNITS` (private there; transforms.rs is read-only). Only used
/// on the `LineBreak` two-tier tail path to detect "the closed prefix already hit the cap, so the
/// full transform never reaches the forming bar" — the byte-identical edge.
const MAX_TRANSFORM_UNITS: usize = 20_000;

/// One populated [`ChartState::transformed`] cache slot.
struct TransformEntry {
    key: TransformKey,
    /// The forming fingerprint `proxy` was built for. `None` = no forming bar.
    forming_fp: Option<FormingFp>,
    /// The returned series: `proxy[..closed_len_in_proxy]` is the immutable CLOSED-prefix
    /// transform, `proxy[closed_len_in_proxy..]` the forming tail.
    proxy: Rc<Vec<Bar>>,
    /// Where the closed-prefix transform ends within `proxy` (two-tier styles: HeikinAshi /
    /// LineBreak — the tail is truncated back to here and rebuilt each forming tick, reusing the
    /// `Rc`'s buffer in place via `Rc::make_mut`, so a forming tick is O(tail), not O(N)).
    /// `usize::MAX` for the fallback styles (whole `proxy` is rebuilt each change — their
    /// `auto_box` reads the forming bar, so no closed sub-result is reusable).
    closed_len_in_proxy: usize,
    /// Structured Kagi/PnF results the drawing needs (computed once here, not re-derived).
    kagi: Option<Rc<Kagi>>,
    pnf: Option<Rc<(Vec<PnFColumn>, f64)>>,
}

/// The shared chart state the UI thread renders.
pub struct ChartState {
    /// closed bars first, then (optionally) the live forming bar as the last element.
    pub bars: Vec<Bar>,
    /// number of CLOSED bars at the front of `bars` — `bars[closed_len..]` is the
    /// forming bar (empty or one element). Closed bars are immutable/append-only;
    /// only the forming bar mutates in place between snapshots.
    pub closed_len: usize,
    /// (min low, max high) over the CLOSED prefix — cached so the render path
    /// extends it with the forming bar in O(1) instead of folding the whole
    /// history every frame. `None` until the first closed bar arrives.
    pub y_ext: Option<(f64, f64)>,
    /// bar indices (== x values) of CLOSED bars whose local wall-clock minute is
    /// :00 — the hourly time-axis gridline positions, cached for the same reason.
    pub hour_marks: Vec<f64>,
    /// bar indices (== x values) of CLOSED bars whose tz-local calendar DATE differs from the
    /// previous bar's — the day/session divider gridline positions (task A3). Index 0 is never
    /// a divider (no previous bar to compare against). Cached identically to `hour_marks`: same
    /// `cache_key`, same append/full-recompute split in [`Self::refresh_caches`].
    pub day_marks: Vec<f64>,
    /// O(1) cache key for the fields above: (closed_len, first ot, last ot, tz identity —
    /// [`DisplayTz::tz_key`]). Valid because closed bars are immutable and append-only, and `tz`
    /// only changes via [`ChartState::set_tz`]. LOAD-BEARING across the whole render pipeline:
    /// `y_ext`, `hour_marks`, `day_marks`, `median_secs`, `hour_step`, `day_step` (task A4),
    /// `vol_cache`, `transform_cache` AND `sync`'s incremental prefix-match all trust that this
    /// quadruple uniquely identifies the closed content (+ display tz) of THIS `ChartState`. A
    /// closed prefix that changes interior content without changing (len, first_ot, last_ot) — a
    /// revised historical bar, or two different series aliased onto one `ChartState` (e.g. a
    /// venue-agnostic `symbol@interval` chart key fed two venues) — goes stale across every cache
    /// at once instead of self-correcting per frame. Guaranteed against by the append-only
    /// `BarSeries.closed` contract + one venue per `symbol@interval` chart key.
    /// `y_ext`/`vol_cache`/`transform_cache` don't actually depend on `tz`; folding it into the
    /// shared key just over-invalidates them on a `set_tz` (harmless — they recompute to the same
    /// value).
    cache_key: (usize, i64, i64, u32),
    /// O(1) cache for [`Self::median_secs`]: the median seconds-per-bar over the closed prefix
    /// (task A4) — see [`median_bar_secs`] for why this stays cheap (bounded to the last ≤512
    /// closed-bar deltas) regardless of total history length. `None` until a `refresh_caches`
    /// call observes ≥2 closed bars. Refreshed alongside `hour_step`/`day_step` at the end of
    /// both work-doing branches of [`Self::refresh_caches`] — NOT the early-return
    /// unchanged-key branch, where the closed prefix can't have changed so the cached value is
    /// still correct. `Cell`-wrapped for the same shared-`&self` reason as `vol_cache`/`tz`
    /// below.
    median_secs: Cell<Option<f64>>,
    /// O(1) cache for [`Self::hour_step`]: `mark_step(&self.hour_marks, 60.0)`, the median
    /// index-gap between consecutive hour marks — the egui_plot grid-step hint task A5 wires up.
    /// Refreshed alongside `median_secs` above. Defaults to `60.0` (one-minute-bar hourly
    /// spacing), matching [`mark_step`]'s own <2-marks fallback, so a getter read before the
    /// first `refresh_caches` call agrees with what that call would produce on an empty chart.
    hour_step: Cell<f64>,
    /// O(1) cache for [`Self::day_step`]: `mark_step(&self.day_marks, 1440.0)`. Refreshed
    /// alongside `median_secs` above. Defaults to `1440.0` (one-minute-bar daily spacing), same
    /// reasoning as `hour_step` above.
    day_step: Cell<f64>,
    /// Display timezone for time-axis labels/hour marks (task A2), read by [`Self::tz`] and
    /// baked into `cache_key` via [`DisplayTz::tz_key`]. `Cell`-wrapped so [`Self::set_tz`] can
    /// mutate it through `&self` — the real render call site (`chart::draw`) holds only
    /// `&ChartState`, same reason as `vol_cache` above. ONE global setting per `ChartState` (see
    /// `tz.rs`'s module doc: marks caches are shared across windows of the same `symbol@interval`
    /// key, so this cannot be per-window). Defaults to [`DisplayTz::Local`] — pre-feature
    /// behavior, zero visual change until the user picks a zone.
    tz: Cell<DisplayTz>,
    /// O(1) cache for [`ChartState::visible_vol_max`]: `((lo, hi, cache_key), value)`.
    /// `Cell`-wrapped rather than a plain field — see [`ChartState::visible_vol_max_shared`]
    /// for why (the real render call sites only ever hold `&ChartState`, never `&mut`).
    /// SINGLE SLOT, and vike-app keys `charts` by `symbol@interval`, so ONE `ChartState` is
    /// shared by every window of that symbol (e.g. the title-bar clone button). Two same-symbol
    /// windows differing in visible range (here) or chart style (`transform_cache`) evict each
    /// other every frame — still byte-identical, just no cache benefit (degrades to the
    /// pre-cache recompute). A 1–2 entry LRU would cover the clone-and-compare case.
    vol_cache: Cell<Option<(VolCacheKey, f64)>>,
    /// Perf-gate telemetry: incremented only on a [`ChartState::visible_vol_max`] cache
    /// miss (a genuine recompute) — unchanged on a cache hit. `Cell` for the same reason
    /// as `vol_cache` above.
    pub vol_recompute_count: Cell<usize>,
    /// `(closed.len(), closed.first().ts, closed.last().ts)` of the `vike_model::Bar`
    /// prefix folded into `bars` as of the last [`sync`] call — the O(1) probe that
    /// lets `sync` tell an unchanged/appended/reloaded source apart without touching
    /// the render buffer. Defaults to `(0, 0, 0)`, matching an empty first sync.
    ///
    /// [`sync`]: ChartState::sync
    synced_source_key: (usize, i64, i64),
    /// perf-gate telemetry: how much rendering [`ChartState::sync`] did on its last call.
    pub last_sync_work: SyncWork,
    /// perf-gate telemetry: how much folding [`ChartState::refresh_caches`] did on its
    /// last call.
    pub last_refresh_work: RefreshWork,
    /// Two-tier transform-style cache (chart-perf T6). `RefCell` for the same reason
    /// [`ChartState::vol_cache`] is `Cell` — the real render call site holds only `&ChartState`
    /// (see [`ChartState::transformed_shared`]); a `Vec<Bar>` can't ride in a `Cell`, so a
    /// `RefCell` + an `Rc` handout is the shared-reference-safe shape.
    transform_cache: RefCell<Option<TransformEntry>>,
    /// Perf-gate telemetry: how many times the CLOSED-prefix transform was (re)computed — i.e.
    /// how many frames had to reprocess the closed bars. Two-tier styles bump it only on a bar
    /// close / style / param change (a forming tick reuses the cached closed prefix); fallback
    /// styles bump it whenever the transform is rebuilt (any close-or-forming change, since their
    /// `auto_box` reads the forming bar); NO style bumps it on a static frame (cache hit). `Cell`
    /// for the same shared-`&self` reason as [`ChartState::vol_recompute_count`].
    pub transform_closed_recompute_count: Cell<usize>,
    /// SP2 (T4) volume-profile overlay cache: `((i0, i1, footprint_len, tick_size_bits),
    /// Rc<VolumeProfile>)`, keyed by [`ProfileCacheKey`]. `RefCell` + `Rc` handout for the same
    /// reason [`ChartState::transform_cache`] is shaped that way — `VolumeProfile` owns a
    /// `Vec<PriceBin>` (can't ride in a `Cell` like [`ChartState::vol_cache`]'s plain `f64`), and
    /// the real render call site (`chart::draw`'s price-pane paint closure) holds only
    /// `&ChartState`.
    profile_cache: RefCell<Option<(ProfileCacheKey, Rc<VolumeProfile>)>>,
    /// Perf-gate telemetry: incremented only on a [`ChartState::visible_profile`] cache miss (a
    /// genuine `VolumeProfile::from_footprints` recompute) — unchanged on a cache hit. `Cell` for
    /// the same shared-`&self` reason as [`ChartState::vol_recompute_count`].
    pub profile_recompute_count: Cell<usize>,
    /// SP3 Task B #2 (SP2 final-review finding B) CVD pane cache: `(CvdCacheKey, Rc<Vec<f64>>)`.
    /// `RefCell` + `Rc` handout for the same reason [`ChartState::profile_cache`] is shaped that
    /// way — `chart::draw`'s CVD sub-pane holds only `&ChartState`. See [`CvdCacheKey`]'s doc for
    /// why a monotonic footprint generation, not the footprint slice's own address, is the sound
    /// identity component here.
    cvd_cache: RefCell<Option<(CvdCacheKey, Rc<Vec<f64>>)>>,
    /// Perf-gate telemetry: incremented only on a [`ChartState::cvd_shared`] cache miss (a
    /// genuine `orderflow::cvd_from_footprints` recompute) — unchanged on a cache hit. `Cell` for
    /// the same shared-`&self` reason as [`ChartState::vol_recompute_count`].
    pub cvd_recompute_count: Cell<usize>,
    pub overlays: BTreeMap<String, Vec<[f64; 2]>>,
    pub symbol: String,
}

impl Default for ChartState {
    /// Hand-written (not `#[derive(Default)]`) so `hour_step`/`day_step` can seed their
    /// non-zero fallback values (task A4) — see those fields' docs. Every other field matches
    /// exactly what `#[derive(Default)]` would have produced.
    fn default() -> Self {
        Self {
            bars: Vec::new(),
            closed_len: 0,
            y_ext: None,
            hour_marks: Vec::new(),
            day_marks: Vec::new(),
            cache_key: (0, 0, 0, 0),
            median_secs: Cell::new(None),
            hour_step: Cell::new(60.0),
            day_step: Cell::new(1440.0),
            tz: Cell::new(DisplayTz::default()),
            vol_cache: Cell::new(None),
            vol_recompute_count: Cell::new(0),
            synced_source_key: (0, 0, 0),
            last_sync_work: SyncWork::default(),
            last_refresh_work: RefreshWork::default(),
            transform_cache: RefCell::new(None),
            transform_closed_recompute_count: Cell::new(0),
            profile_cache: RefCell::new(None),
            profile_recompute_count: Cell::new(0),
            cvd_cache: RefCell::new(None),
            cvd_recompute_count: Cell::new(0),
            overlays: BTreeMap::new(),
            symbol: String::new(),
        }
    }
}

/// Bar indices whose open-time falls on a `tz` wall-clock hour boundary
/// (minute == 0) — the chart's hourly gridline marks. For any fixed-offset
/// timezone (including :30/:45 offsets) exactly one mark occurs per 60
/// one-minute bars.
pub fn hour_mark_indices(bars: &[Bar], tz: DisplayTz) -> Vec<f64> {
    use chrono::Timelike;
    bars.iter()
        .enumerate()
        .filter_map(|(i, b)| {
            crate::tz::to_naive(b.ot, tz).is_some_and(|dt| dt.minute() == 0).then_some(i as f64)
        })
        .collect()
}

/// Bar indices (== x values) where the tz-local calendar DATE changes vs the previous
/// bar — the day/session divider positions. Index 0 is never a divider.
pub fn day_mark_indices(bars: &[Bar], tz: DisplayTz) -> Vec<f64> {
    let mut out = Vec::new();
    let mut prev_date = None;
    for (i, b) in bars.iter().enumerate() {
        let date = crate::tz::to_naive(b.ot, tz).map(|dt| dt.date());
        if i > 0 && date.is_some() && date != prev_date {
            out.push(i as f64);
        }
        prev_date = date;
    }
    out
}

/// Median seconds-per-bar over the last ≤512 CLOSED-bar deltas. None when <2 bars.
/// Bounded sample keeps the (bar-close-only) recompute O(512 log 512).
pub fn median_bar_secs(bars: &[Bar]) -> Option<f64> {
    if bars.len() < 2 {
        return None;
    }
    let tail = &bars[bars.len().saturating_sub(513)..];
    let mut deltas: Vec<i64> =
        tail.windows(2).map(|w| w[1].ot - w[0].ot).filter(|d| *d > 0).collect();
    if deltas.is_empty() {
        return None;
    }
    deltas.sort_unstable();
    Some(deltas[deltas.len() / 2] as f64 / 1000.0)
}

/// Coarse bar-duration tier derived from [`median_bar_secs`] — which axis-label/gridline-density
/// regime (task A5) a chart's inferred bar interval falls into: sub-minute tick/second bars,
/// minute-ish bars, or hour-plus/daily bars.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimeGrain {
    Sub60,
    Minute,
    HourPlus,
}

/// Bucket a [`median_bar_secs`] reading into a [`TimeGrain`]. `None` (too little history to
/// infer — fewer than 2 closed bars) defaults to `Minute`, the implicit pre-A4 assumption.
pub fn grain(median_secs: Option<f64>) -> TimeGrain {
    match median_secs {
        Some(s) if s < 60.0 => TimeGrain::Sub60,
        Some(s) if s >= 3600.0 => TimeGrain::HourPlus,
        _ => TimeGrain::Minute,
    }
}

/// Median index-gap between consecutive marks — the egui_plot `GridMark` step hint. `fallback`
/// covers <2 marks (nothing to derive a gap from: a brand-new chart, or a visible window with no
/// hour/day boundary in it yet).
pub fn mark_step(marks: &[f64], fallback: f64) -> f64 {
    if marks.len() < 2 {
        return fallback;
    }
    let mut gaps: Vec<f64> = marks.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_unstable_by(|a, b| a.total_cmp(b));
    gaps[gaps.len() / 2]
}

/// Map one wire-format `vike_model::Bar` (venue/core-side OHLCV) to the render-model
/// `Bar` at bar-index `i`. The single source of truth for that mapping — both
/// `ChartState::sync`'s fast paths and its full-rebuild fallback go through this.
fn render_bar(i: usize, b: &vike_model::Bar) -> Bar {
    Bar { t: i as f64, ot: b.ts, o: b.open, h: b.high, l: b.low, c: b.close, v: b.volume }
}

impl ChartState {
    /// Fold the core's per-series bars into `self.bars` incrementally. `closed` is the
    /// immutable append-only closed prefix (vike-model), `forming` the live bar (if any).
    /// Byte-identical to a full clear+rebuild; does O(delta) work, not O(history).
    /// Records `self.last_sync_work` for the perf gate. Calls `refresh_caches()`.
    pub fn sync(
        &mut self,
        closed: &std::sync::Arc<Vec<vike_model::Bar>>,
        forming: Option<&vike_model::Bar>,
    ) {
        let synced = self.synced_source_key;
        let key =
            (closed.len(), closed.first().map_or(0, |b| b.ts), closed.last().map_or(0, |b| b.ts));

        let closed_rerendered = if key == synced {
            // closed prefix unchanged (including the trivial empty/first-call case) —
            // drop the stale forming slot (if any); nothing to rerender.
            self.bars.truncate(self.closed_len);
            0
        } else if closed.len() > synced.0
            && synced.0 >= 1
            && closed.first().map_or(0, |b| b.ts) == synced.1
            && closed[synced.0 - 1].ts == synced.2
        {
            // append: the previously-synced prefix is an unchanged prefix of `closed` —
            // render only the newly-closed tail, in place of the prior forming slot.
            self.bars.truncate(self.closed_len);
            for (i, b) in closed[synced.0..].iter().enumerate() {
                self.bars.push(render_bar(synced.0 + i, b));
            }
            self.closed_len = closed.len();
            closed.len() - synced.0
        } else {
            // shrink / first-ts change / boundary mismatch / first sync → full rebuild.
            self.bars.clear();
            for (i, b) in closed.iter().enumerate() {
                self.bars.push(render_bar(i, b));
            }
            self.closed_len = closed.len();
            closed.len()
        };

        if let Some(f) = forming {
            let idx = self.bars.len();
            self.bars.push(render_bar(idx, f));
        }

        self.synced_source_key = key;
        self.last_sync_work = SyncWork { closed_rerendered, forming_rerendered: forming.is_some() };
        // O(1) key probe per snapshot; O(N) folds only when a bar actually closed.
        self.refresh_caches();
    }

    /// Set the display timezone for time-axis labels/hour marks (task A2). Takes effect from the
    /// next [`Self::sync`]/[`Self::refresh_caches`] call: `tz` is baked into `cache_key`, so a
    /// change here forces the next call to take the full `hour_marks` recompute path (see that
    /// field's doc) rather than trusting the (now stale, wrong-tz) cached marks.
    pub fn set_tz(&self, tz: DisplayTz) {
        self.tz.set(tz);
    }

    /// The current display timezone (default [`DisplayTz::Local`] — see [`Self::tz`]'s field doc).
    pub fn tz(&self) -> DisplayTz {
        self.tz.get()
    }

    /// Refresh the O(1)-probe caches (`y_ext`, `hour_marks`, `day_marks`, and — task A4 —
    /// `median_secs`/`hour_step`/`day_step`) if the closed prefix (or the display tz) changed
    /// since the last call. Cheap enough to call once per snapshot: the key compare is O(1); on
    /// a bar CLOSE (or reload, or a `set_tz`) it does O(delta) work, not O(history) — closed
    /// bars are immutable/append-only, so on a plain append only the newly-closed tail needs a
    /// mark/extent check; the rest of `y_ext`/`hour_marks`/`day_marks` is already correct and is
    /// left alone (the A4 trio re-derives in O(≤512) / O(mark count) regardless, still far
    /// cheaper than an O(history) fold). Records `self.last_refresh_work` for the perf gate.
    pub fn refresh_caches(&mut self) {
        let tz = self.tz.get();
        let closed = &self.bars[..self.closed_len.min(self.bars.len())];
        let key = (
            closed.len(),
            closed.first().map_or(0, |b| b.ot),
            closed.last().map_or(0, |b| b.ot),
            tz.tz_key(),
        );
        let prev_key = self.cache_key;
        if key == prev_key {
            self.last_refresh_work = RefreshWork { recomputed: 0 };
            return;
        }
        let prev_len = prev_key.0;
        let recomputed = if prev_len >= 1
            && closed.len() > prev_len
            && closed.first().map_or(0, |b| b.ot) == prev_key.1
            && closed[prev_len - 1].ot == prev_key.2
            && key.3 == prev_key.3
        {
            // append: the previously-cached prefix is an unchanged prefix of `closed`, on the
            // SAME tz (the `key.3 == prev_key.3` check above) — extend y_ext/hour_marks/day_marks
            // with only the newly-closed tail. `hour_mark_indices` returns indices RELATIVE to
            // `add`, so offset each by `prev_len` to land on the tail's true absolute bar index.
            // Without the tz check, a `set_tz` landing in the same call as a real append would
            // wrongly keep the OLD-tz `hour_marks`/`day_marks` prefix and append a NEW-tz tail —
            // fall through to the full recompute below instead.
            let add = &closed[prev_len..];
            let add_lo = add.iter().map(|b| b.l).fold(f64::INFINITY, f64::min);
            let add_hi = add.iter().map(|b| b.h).fold(f64::NEG_INFINITY, f64::max);
            let (old_lo, old_hi) =
                self.y_ext.expect("append implies a prior non-empty y_ext (prev_len >= 1)");
            self.y_ext = Some((old_lo.min(add_lo), old_hi.max(add_hi)));
            self.hour_marks
                .extend(hour_mark_indices(add, tz).into_iter().map(|m| m + prev_len as f64));
            // `day_mark_indices` needs the bar BEFORE `add[0]` to decide whether `add[0]` itself
            // is a divider (its date vs. the previous CLOSED bar's) — so start the slice one bar
            // earlier, at `prev_len - 1` (the sentinel), instead of passing `add` alone.
            // `day_mark_indices` never flags index 0 of the slice it's given (no previous bar
            // within the slice to compare against), so the sentinel itself can never spuriously
            // appear — every returned index is >= 1, i.e. genuinely within `add`. Offset by
            // `prev_len - 1` (not `prev_len`, to account for the sentinel) to land on the tail's
            // true absolute bar index.
            self.day_marks.extend(
                day_mark_indices(&closed[prev_len - 1..], tz)
                    .into_iter()
                    .map(|m| m + (prev_len - 1) as f64),
            );
            add.len()
        } else {
            // shrink / first-ot change / boundary mismatch / tz change / prev_len == 0 → full
            // recompute.
            self.y_ext = if closed.is_empty() {
                None
            } else {
                let lo = closed.iter().map(|b| b.l).fold(f64::INFINITY, f64::min);
                let hi = closed.iter().map(|b| b.h).fold(f64::NEG_INFINITY, f64::max);
                Some((lo, hi))
            };
            self.hour_marks = hour_mark_indices(closed, tz);
            self.day_marks = day_mark_indices(closed, tz);
            closed.len()
        };
        self.cache_key = key;
        // task A4: bar-duration inference + grid-step hints, refreshed alongside the marks
        // above — cheap even on a full recompute (`median_bar_secs` is bounded to its own
        // ≤512-delta tail; `mark_step` folds the just-recomputed mark vectors).
        self.median_secs.set(median_bar_secs(closed));
        self.hour_step.set(mark_step(&self.hour_marks, 60.0));
        self.day_step.set(mark_step(&self.day_marks, 1440.0));
        self.last_refresh_work = RefreshWork { recomputed };
    }

    /// Median seconds-per-bar over the closed prefix (task A4), as of the last
    /// [`Self::refresh_caches`] call — see [`median_bar_secs`].
    pub fn median_secs(&self) -> Option<f64> {
        self.median_secs.get()
    }

    /// Median index-gap between consecutive [`Self::hour_marks`] entries (task A4) — the
    /// egui_plot grid-step hint for the hour-gridline layer. `60.0` (one-minute-bar hourly
    /// spacing) until at least two hour marks exist.
    pub fn hour_step(&self) -> f64 {
        self.hour_step.get()
    }

    /// Median index-gap between consecutive [`Self::day_marks`] entries (task A4) — the
    /// egui_plot grid-step hint for the day/session-divider layer. `1440.0` (one-minute-bar
    /// daily spacing) until at least two day marks exist.
    pub fn day_step(&self) -> f64 {
        self.day_step.get()
    }

    /// Max CLOSED-bar volume over the visible index window `[lo, hi)`, cached by
    /// `(lo, hi, cache_key)` — O(1) unless that key changed since the last call (the
    /// window moved, or `cache_key` bumped from a bar close), in which case it's a plain
    /// O(visible) fold, same as today's per-frame `visible_slice(...).map(|b|
    /// b.v).fold(0.0, f64::max)`. `hi` is clamped to the closed prefix (`hi.min(closed_len)`);
    /// `lo` is clamped down to that if it would otherwise exceed it. Empty window → `0.0`
    /// (the fold's zero identity). The forming bar (if any) is deliberately NOT included —
    /// callers `.max()` it in themselves when it's visible (index `closed_len` in `[lo,
    /// hi)`), reproducing today's `visible_slice`-over-`bars` fold (which sees the forming
    /// bar as just the last element) exactly, in O(1).
    ///
    /// Takes `&mut self` per the task interface (and for the RED/GREEN unit tests below);
    /// forwards to [`Self::visible_vol_max_shared`], the actual (`&self`) implementation —
    /// see that method's doc for why.
    pub fn visible_vol_max(&mut self, lo: usize, hi: usize) -> f64 {
        self.visible_vol_max_shared(lo, hi)
    }

    /// Shared-reference twin of [`Self::visible_vol_max`] — identical cache, identical
    /// semantics, but callable through `&ChartState`. This is the one both real call sites
    /// (`render.rs`'s `draw_volume_candles` for the `VolumeCandles` style, and `chart.rs`'s
    /// Volume sub-pane) actually use: `chart::draw` receives `state: &ChartState`
    /// (`ChartInputs::state`), never `&mut` — and making it `&mut` would ripple into
    /// `vike-app` (the `self.charts: HashMap<..>` shared borrow held across the whole
    /// per-window loop, plus a `bars_slice` read after the draw call in the same
    /// iteration) for a chart-perf task scoped to `vike-chart`. The cache fields are
    /// `Cell`-wrapped so this can mutate them through `&self`; that's pure memoization (no
    /// externally observable state changes) and `ChartState` is UI-thread-only (never
    /// shared across threads), so `Cell` — not `RefCell`/`Mutex` — is the right tool.
    pub fn visible_vol_max_shared(&self, lo: usize, hi: usize) -> f64 {
        let key = (lo, hi, self.cache_key);
        if let Some((cached_key, v)) = self.vol_cache.get() {
            if cached_key == key {
                return v;
            }
        }
        let closed = self.closed_len.min(self.bars.len());
        let hi_c = hi.min(closed);
        let lo_c = lo.min(hi_c);
        let vmax = self.bars[lo_c..hi_c].iter().map(|b| b.v).fold(0.0_f64, f64::max);
        self.vol_cache.set(Some((key, vmax)));
        self.vol_recompute_count.set(self.vol_recompute_count.get() + 1);
        vmax
    }

    // === SP2 T4: visible-range volume-profile overlay cache ================================

    /// The volume-at-price profile (POC + 70% value area) over footprint bars `[i0, i1]`
    /// (inclusive), cached by [`ProfileCacheKey`] — SP2's volume-profile overlay (T4): a static
    /// frame (unchanged visible window, footprint length, and resolved tick size) is an O(1)
    /// `Rc` clone instead of [`vike_orderflow::VolumeProfile::from_footprints`]'s
    /// `BTreeMap`-folding recompute. `footprints` is `ChartInputs::footprint`, passed in fresh
    /// each call rather than stored on `ChartState` — see [`ProfileCacheKey`]'s doc for why
    /// length is the change-proxy here instead of `cache_key`.
    ///
    /// Takes `&mut self` per the sibling cache methods' interface (see
    /// [`Self::visible_vol_max`]'s doc for why); forwards to [`Self::visible_profile_shared`],
    /// the actual (`&self`) implementation the real render call site (`chart::draw`, holding
    /// only `&ChartState`) uses.
    pub fn visible_profile(
        &mut self,
        footprints: &[FootprintBar],
        tick_size: f64,
        i0: usize,
        i1: usize,
    ) -> Rc<VolumeProfile> {
        self.visible_profile_shared(footprints, tick_size, i0, i1)
    }

    /// Shared-reference twin of [`Self::visible_profile`] — the one `chart::draw` actually
    /// calls.
    pub fn visible_profile_shared(
        &self,
        footprints: &[FootprintBar],
        tick_size: f64,
        i0: usize,
        i1: usize,
    ) -> Rc<VolumeProfile> {
        let key: ProfileCacheKey = (i0, i1, footprints.len(), tick_size.to_bits());
        let mut slot = self.profile_cache.borrow_mut();
        if let Some((cached_key, prof)) = slot.as_ref() {
            if *cached_key == key {
                return prof.clone();
            }
        }
        self.profile_recompute_count.set(self.profile_recompute_count.get() + 1);
        let prof = Rc::new(VolumeProfile::from_footprints(footprints, tick_size, i0, i1));
        *slot = Some((key, prof.clone()));
        prof
    }

    // === SP3 Task B #2 (SP2 final-review finding B): CVD pane recompute cache ==============

    /// Cumulative volume delta over the WHOLE footprint substrate (`orderflow::cvd_from_footprints`
    /// — unlike the visible-range volume-profile overlay above, the CVD pane plots the full
    /// running series, so it can't be windowed the same way), cached by [`CvdCacheKey`]. A static
    /// frame (the footprint generation and length both unchanged) is an O(1) `Rc` clone instead of
    /// re-folding every bar's cells — the fix for `chart.rs`'s CVD sub-pane, which used to call
    /// `cvd_from_footprints` fresh every frame (SP2 v1; see that call site's old comment).
    /// `generation` is `ChartInputs::footprint_gen` (vike-app's `OrderflowAgg::generation()`) —
    /// see [`CvdCacheKey`]'s doc for why this replaced the footprint slice's own `(ptr, len)`
    /// (SP3 TB-fix, a post-Task-B review finding, MEDIUM: a narrow ABA-staleness gap across a CVD
    /// toggle-off/on cycle). The real render call site (`chart::draw`) holds only `&ChartState`,
    /// so — same shape as [`Self::visible_profile_shared`] — this lives directly behind `&self`
    /// interior mutability rather than needing a separate `&mut self` entry point.
    pub fn cvd_shared(&self, footprints: &[FootprintBar], generation: u64) -> Rc<Vec<f64>> {
        let key: CvdCacheKey = (generation, footprints.len());
        let mut slot = self.cvd_cache.borrow_mut();
        if let Some((cached_key, v)) = slot.as_ref() {
            if *cached_key == key {
                return v.clone();
            }
        }
        self.cvd_recompute_count.set(self.cvd_recompute_count.get() + 1);
        let v = Rc::new(crate::orderflow::cvd_from_footprints(footprints));
        *slot = Some((key, v.clone()));
        v
    }

    // === chart-perf T6: two-tier transform-style recompute cache ===========================

    /// The transformed bar series for `style` (HeikinAshi/Renko/Range/LineBreak/Kagi/PointFigure),
    /// caching the CLOSED-prefix transform keyed by `(style, params, cache_key)` and recomputing
    /// only the forming-affected tail each call. Byte/value-identical to a full transform of
    /// (closed + forming).
    ///
    /// Returns an `Rc<Vec<Bar>>` rather than a `&[Bar]`: the real render call site
    /// (`chart::draw`) holds only `&ChartState` (`ChartInputs::state`), so — exactly like
    /// [`Self::visible_vol_max`] — the work lives in [`Self::transformed_shared`] behind interior
    /// mutability, and a borrow can't escape the `RefCell`. The `Rc` handout is O(1) to clone on a
    /// cache hit; see that method for the two-tier design.
    pub fn transformed(&mut self, style: ChartStyle, params: TransformParams) -> Rc<Vec<Bar>> {
        self.transformed_shared(style, params)
    }

    /// Shared-reference twin of [`Self::transformed`] — the one `chart::draw` actually calls
    /// (it receives `state: &ChartState`, never `&mut`).
    ///
    /// Two tiers, both gated by the byte-identical equivalence test:
    /// - **Fast path (all styles):** `(key, forming_fp)` both match → return the cached `Rc` in
    ///   O(1). Kills the per-frame recompute on static frames (the dominant case: a GUI redraw
    ///   with no new tick).
    /// - **Two-tier (HeikinAshi, LineBreak):** the CLOSED-prefix transform is a pure function of
    ///   the immutable closed bars, cached under `key` and rebuilt only on a bar close / style /
    ///   param change. A forming tick reuses it and rebuilds ONLY the forming tail — one HA bar,
    ///   or LineBreak's ≤1 appended block from the last-`n`-block window.
    /// - **Fallback (Renko/Range/Kagi/PointFigure):** these derive their box/reversal size from
    ///   `transforms::auto_box` over ALL bars (the forming bar included), so a forming tick shifts
    ///   every brick/column — no closed sub-result is byte-reusable. They full-recompute
    ///   (closed+forming) on any change, but still serve static frames from the cache.
    pub fn transformed_shared(&self, style: ChartStyle, params: TransformParams) -> Rc<Vec<Bar>> {
        let key = TransformKey { style, params, cache_key: self.cache_key };
        let closed_end = self.closed_len.min(self.bars.len());
        let forming: Option<&Bar> = self.bars.get(closed_end);
        let ffp: Option<FormingFp> = forming.map(forming_fingerprint);

        let mut slot = self.transform_cache.borrow_mut();

        // Fast path: fully cached (nothing changed) → O(1) `Rc` clone.
        if let Some(e) = slot.as_ref() {
            if e.key == key && e.forming_fp == ffp {
                return e.proxy.clone();
            }
        }

        let all = &self.bars[..closed_end + usize::from(forming.is_some())];
        let first_ot = self.bars.first().map_or(0, |b| b.ot);

        // Fallback styles (Renko/Range/Kagi/PointFigure, and any non-transform probe): their
        // `auto_box` reads the forming bar, so no closed sub-result is byte-reusable — full
        // recompute (closed+forming) every change (static frames still hit the fast path above).
        if !matches!(style, ChartStyle::HeikinAshi | ChartStyle::LineBreak) {
            self.bump_closed_recompute();
            let (p, kagi, pnf) = transform_full(style, params, all, first_ot);
            let proxy = Rc::new(p);
            let out = proxy.clone();
            *slot = Some(TransformEntry {
                key,
                forming_fp: ffp,
                proxy,
                closed_len_in_proxy: usize::MAX,
                kagi,
                pnf,
            });
            return out;
        }

        // Two-tier styles: reuse the previous `proxy`'s buffer in place. In steady state the caller
        // has dropped last frame's returned `Rc`, so `Rc::make_mut` mutates without cloning — a
        // forming tick truncates the old tail (O(tail)) and rebuilds only the forming tail; the
        // immutable closed prefix `[..closed_len_in_proxy]` is rebuilt only on a `key` change.
        let prev = slot.take();
        let key_changed = prev.as_ref().is_none_or(|e| e.key != key);
        let prev_closed_len = prev.as_ref().map_or(0, |e| e.closed_len_in_proxy);
        let mut buf = prev.map_or_else(|| Rc::new(Vec::new()), |e| e.proxy);
        let vec = Rc::make_mut(&mut buf);

        let closed_len_in_proxy = if key_changed {
            self.bump_closed_recompute();
            let closed_bars = &self.bars[..closed_end];
            rebuild_closed_into(vec, style, params, closed_bars);
            vec.len()
        } else {
            vec.truncate(prev_closed_len);
            prev_closed_len
        };
        build_tail_into(vec, style, params, forming, all);

        let out = buf.clone();
        *slot = Some(TransformEntry {
            key,
            forming_fp: ffp,
            proxy: buf,
            closed_len_in_proxy,
            kagi: None,
            pnf: None,
        });
        out
    }

    /// The structured Kagi result (`prices`/`thick`) for the drawing, populated by the most recent
    /// [`Self::transformed_shared`] call for the `Kagi` style this frame. `None` for other styles.
    pub fn cached_kagi_shared(&self) -> Option<Rc<Kagi>> {
        self.transform_cache.borrow().as_ref().and_then(|e| e.kagi.clone())
    }

    /// The structured Point & Figure result (`columns`, `box`) for the drawing, populated by the
    /// most recent [`Self::transformed_shared`] call for the `PointFigure` style this frame.
    pub fn cached_pnf_shared(&self) -> Option<Rc<(Vec<PnFColumn>, f64)>> {
        self.transform_cache.borrow().as_ref().and_then(|e| e.pnf.clone())
    }

    fn bump_closed_recompute(&self) {
        self.transform_closed_recompute_count.set(self.transform_closed_recompute_count.get() + 1);
    }
}

/// The full (non-incremental) transform of `bars` for `style`, mirroring `chart.rs`'s `owned`
/// construction exactly — the ground truth for the fallback styles and the closed-prefix build of
/// the two-tier styles. Also returns the structured Kagi/PnF the drawing consumes.
#[allow(clippy::type_complexity)]
fn transform_full(
    style: ChartStyle,
    params: TransformParams,
    bars: &[Bar],
    first_ot: i64,
) -> (Vec<Bar>, Option<Rc<Kagi>>, Option<Rc<(Vec<PnFColumn>, f64)>>) {
    use ChartStyle::*;
    match style {
        HeikinAshi => (heikin_ashi(bars), None, None),
        Renko => (transforms::reindex(transforms::renko(bars)), None, None),
        Range => (transforms::reindex(transforms::range_bars(bars)), None, None),
        LineBreak => {
            (transforms::reindex(transforms::line_break(bars, params.line_break_n)), None, None)
        }
        Kagi => {
            let k = transforms::kagi(bars);
            let proxy = transforms::reindex(
                k.prices
                    .iter()
                    .map(|&p| Bar { t: 0.0, ot: first_ot, o: p, h: p, l: p, c: p, v: 0.0 })
                    .collect(),
            );
            (proxy, Some(Rc::new(k)), None)
        }
        PointFigure => {
            let (cols, box_) = transforms::point_and_figure(bars, params.pnf_reversal);
            let proxy = transforms::reindex(
                cols.iter()
                    .map(|c| Bar {
                        t: 0.0,
                        ot: first_ot,
                        o: c.bottom,
                        h: c.top,
                        l: c.bottom,
                        c: c.top,
                        v: 0.0,
                    })
                    .collect(),
            );
            (proxy, None, Some(Rc::new((cols, box_))))
        }
        // Non-transform styles never route here (the call site only calls `transformed` for the
        // six), but a `key`-invalidation probe (e.g. a style switch) can — an empty proxy is right.
        _ => (Vec::new(), None, None),
    }
}

/// Rebuild the CLOSED-prefix transform of `closed` for a two-tier style into `vec` (cleared
/// first). Byte-identical to the closed-prefix slice of `transform_full(style, closed+forming)`
/// because both transforms process bars left-to-right and the forming bar is strictly last.
fn rebuild_closed_into(
    vec: &mut Vec<Bar>,
    style: ChartStyle,
    params: TransformParams,
    closed: &[Bar],
) {
    vec.clear();
    match style {
        ChartStyle::HeikinAshi => {
            vec.reserve(closed.len());
            for b in closed {
                let prev = vec.last().copied();
                vec.push(heikin_ashi_bar(prev.as_ref(), b));
            }
        }
        ChartStyle::LineBreak => {
            // `line_break` already caps at its own `MAX_UNITS`; reindex in place.
            vec.extend(transforms::line_break(closed, params.line_break_n));
            for (i, b) in vec.iter_mut().enumerate() {
                b.t = i as f64;
            }
        }
        _ => {}
    }
}

/// Append the forming-affected tail onto `vec` (already holding the immutable closed-prefix
/// transform). Byte-identical to a full transform of (closed + forming) — the equivalence test is
/// the gate.
fn build_tail_into(
    vec: &mut Vec<Bar>,
    style: ChartStyle,
    params: TransformParams,
    forming: Option<&Bar>,
    all: &[Bar],
) {
    match style {
        // Per-bar recurrence: the forming HA bar depends only on the last closed HA bar + the raw
        // forming bar.
        ChartStyle::HeikinAshi => {
            if let Some(f) = forming {
                let prev = vec.last().copied();
                vec.push(heikin_ashi_bar(prev.as_ref(), f));
            }
        }
        // LineBreak has no `auto_box`, so the closed blocks are stable and the forming bar appends
        // AT MOST ONE block, decided by the last-`n`-block window (the same test `line_break` runs
        // per bar). The closed proxy carries each block as h=top / l=bottom, so the window folds
        // straight off `vec` — no need to keep the raw blocks. Falls back to a full recompute for
        // the degenerate cases the incremental can't reproduce (no closed blocks yet, or the closed
        // prefix already hit the unit cap — both unreachable for a normal multi-hundred-bar chart).
        ChartStyle::LineBreak => {
            let n = params.line_break_n;
            match forming {
                None => {}
                // The closed prefix already hit `line_break`'s unit cap, so its per-bar loop broke
                // BEFORE reaching the (last) forming bar — the forming bar contributes no block.
                Some(_) if vec.len() >= MAX_TRANSFORM_UNITS => {}
                // Normal path: the forming bar appends AT MOST ONE block, decided by the
                // last-`n`-block window read straight off `vec` (h=top / l=bottom per block).
                Some(f) if !vec.is_empty() => {
                    let len = vec.len();
                    let start = len.saturating_sub(n);
                    let hi = vec[start..].iter().map(|b| b.h).fold(f64::MIN, f64::max);
                    let lo = vec[start..].iter().map(|b| b.l).fold(f64::MAX, f64::min);
                    let last = vec[len - 1];
                    let c = f.c;
                    if c > hi {
                        vec.push(Bar {
                            t: len as f64,
                            ot: f.ot,
                            o: last.h,
                            h: c,
                            l: last.h,
                            c,
                            v: 0.0,
                        });
                    } else if c < lo {
                        vec.push(Bar {
                            t: len as f64,
                            ot: f.ot,
                            o: last.l,
                            h: last.l,
                            l: c,
                            c,
                            v: 0.0,
                        });
                    }
                }
                // Degenerate: no closed blocks yet (closed prefix < 2 bars or perfectly flat), so
                // the forming bar's block can seed off `prev_close` — recompute the full transform.
                Some(_) => {
                    vec.clear();
                    vec.extend(transforms::reindex(transforms::line_break(all, n)));
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(i: usize, l: f64, h: f64) -> Bar {
        Bar {
            t: i as f64,
            // one-minute bars starting at an arbitrary epoch minute
            ot: 1_700_000_040_000 + i as i64 * 60_000,
            o: (l + h) / 2.0,
            h,
            l,
            c: (l + h) / 2.0,
            v: 1.0,
        }
    }

    /// A [`Bar`] at bar-index `i` with an explicit open-time `ot` (ms) — for tz-mark tests that
    /// need specific wall-clock instants rather than [`bar`]'s fixed one-minute-epoch formula.
    fn bar_at(i: usize, ot: i64) -> Bar {
        Bar { t: i as f64, ot, o: 1.0, h: 2.0, l: 1.0, c: 1.5, v: 1.0 }
    }

    #[test]
    fn hour_marks_one_per_hour_for_minute_bars() {
        // 180 one-minute bars = 3 full hours → exactly 3 hourly marks, 60 apart,
        // for ANY fixed-offset local timezone (minute-of-hour repeats mod 60).
        let bars: Vec<Bar> = (0..180).map(|i| bar(i, 1.0, 2.0)).collect();
        let marks = hour_mark_indices(&bars, DisplayTz::Local);
        assert_eq!(marks.len(), 3, "3 hours of minute bars → 3 marks");
        assert_eq!(marks[1] - marks[0], 60.0);
        assert_eq!(marks[2] - marks[1], 60.0);
        for m in marks {
            assert!((0.0..180.0).contains(&m));
        }
    }

    /// `hour_mark_indices` re-evaluates minute==0 in the REQUESTED tz, not always local/UTC —
    /// the same instants land on different bar indices depending on `tz`.
    #[test]
    fn hour_marks_follow_display_tz() {
        // bars every 15 min from 11:30 UTC: ots at 11:30, 11:45, 12:00, 12:15 UTC.
        let base = chrono::DateTime::parse_from_rfc3339("2026-07-11T11:30:00Z")
            .unwrap()
            .timestamp_millis();
        let bars: Vec<Bar> = (0..4).map(|i| bar_at(i, base + i as i64 * 900_000)).collect();
        // UTC: only 12:00 (index 2) has minute==0.
        assert_eq!(hour_mark_indices(&bars, DisplayTz::Utc), vec![2.0]);
        // Kolkata (+5:30): 11:30 UTC == 17:00 local → index 0 (and 12:30 would be next; not
        // present).
        assert_eq!(hour_mark_indices(&bars, DisplayTz::Named(chrono_tz::Asia::Kolkata)), vec![0.0]);
    }

    #[test]
    fn day_marks_at_local_midnight_and_move_with_tz() {
        // Hourly bars 21:00 UTC Jul-10 .. 03:00 UTC Jul-11 (7 bars).
        let base = chrono::DateTime::parse_from_rfc3339("2026-07-10T21:00:00Z")
            .unwrap()
            .timestamp_millis();
        let bars: Vec<Bar> = (0..7).map(|i| bar_at(i, base + i as i64 * 3_600_000)).collect();
        // UTC: date flips at 00:00 UTC == index 3. Never index 0.
        assert_eq!(day_mark_indices(&bars, DisplayTz::Utc), vec![3.0]);
        // Tokyo (+9): 21:00 UTC Jul-10 == 06:00 Jul-11 local; prior local midnight is before bar 0
        // → no flip until 15:00 UTC — not in range. So NO day mark in Tokyo for this window.
        assert_eq!(
            day_mark_indices(&bars, DisplayTz::Named(chrono_tz::Asia::Tokyo)),
            Vec::<f64>::new()
        );
        // New York (-4, DST): midnight local == 04:00 UTC — not in range either (all 7 bars land
        // on NY-local Jul-10).
        assert_eq!(
            day_mark_indices(&bars, DisplayTz::Named(chrono_tz::America::New_York)),
            Vec::<f64>::new()
        );
    }

    #[test]
    fn day_marks_append_fastpath_equals_full_recompute() {
        // 3 days of 4h bars, synced in two chunks — cache after incremental == full recompute.
        let base = chrono::DateTime::parse_from_rfc3339("2026-07-09T02:00:00Z")
            .unwrap()
            .timestamp_millis();
        let wire: Vec<vike_model::Bar> =
            (0..18).map(|i| wire_bar_at(base + i as i64 * 4 * 3_600_000)).collect();
        let mut incremental = ChartState::default();
        incremental.set_tz(DisplayTz::Utc);
        incremental.sync(&std::sync::Arc::new(wire[..10].to_vec()), None);
        incremental.sync(&std::sync::Arc::new(wire.clone()), None); // append path
        let mut full = ChartState::default();
        full.set_tz(DisplayTz::Utc);
        full.sync(&std::sync::Arc::new(wire.clone()), None);
        assert_eq!(incremental.day_marks, full.day_marks);
        assert!(!full.day_marks.is_empty());
    }

    /// Carried Minor from A2's review: a `set_tz` immediately followed, in the SAME `sync` call,
    /// by newly-appended closed bars must NOT take the append fast-path under the OLD tz —
    /// `refresh_caches`'s `key.3 == prev_key.3` guard must force the full recompute so both
    /// `hour_marks` and `day_marks` end up computed entirely under the NEW tz, matching a fresh
    /// `ChartState` synced directly to the same final series under that tz. Uses 15-min bars
    /// spanning a Kolkata (+5:30) local-midnight crossing: Utc and Kolkata disagree on which of
    /// the first 5 bars land on the hour (index 2 vs. index 4), so a stale Utc-computed prefix
    /// surviving into a Kolkata-keyed result is directly observable via `hour_marks`.
    #[test]
    fn tz_change_plus_append_forces_full_recompute() {
        let base = chrono::DateTime::parse_from_rfc3339("2026-07-11T11:30:00Z")
            .unwrap()
            .timestamp_millis();
        let wire: Vec<vike_model::Bar> =
            (0..41).map(|i| wire_bar_at(base + i as i64 * 900_000)).collect();

        let mut cs = ChartState::default();
        cs.set_tz(DisplayTz::Utc);
        cs.sync(&std::sync::Arc::new(wire[..5].to_vec()), None);

        // ONE step: change tz AND append the rest of the series.
        cs.set_tz(DisplayTz::Named(chrono_tz::Asia::Kolkata));
        cs.sync(&std::sync::Arc::new(wire.clone()), None);

        let mut fresh = ChartState::default();
        fresh.set_tz(DisplayTz::Named(chrono_tz::Asia::Kolkata));
        fresh.sync(&std::sync::Arc::new(wire.clone()), None);

        assert_eq!(
            cs.hour_marks, fresh.hour_marks,
            "hour_marks must match a fresh Kolkata recompute"
        );
        assert_eq!(cs.day_marks, fresh.day_marks, "day_marks must match a fresh Kolkata recompute");
        assert!(!fresh.day_marks.is_empty(), "sanity: window must cross a real Kolkata midnight");
    }

    #[test]
    fn grain_tiers_and_median() {
        let mk = |secs: i64, n: i64| -> Vec<Bar> {
            (0..n).map(|i| bar_at(i as usize, i * secs * 1000)).collect()
        };
        assert_eq!(median_bar_secs(&mk(1, 100)), Some(1.0));
        assert_eq!(median_bar_secs(&mk(60, 100)), Some(60.0));
        assert_eq!(median_bar_secs(&mk(3600, 50)), Some(3600.0));
        assert_eq!(median_bar_secs(&[]), None);
        assert_eq!(median_bar_secs(&mk(60, 1)), None); // one bar → no delta
        assert_eq!(grain(Some(1.0)), TimeGrain::Sub60);
        assert_eq!(grain(Some(60.0)), TimeGrain::Minute);
        assert_eq!(grain(Some(3600.0)), TimeGrain::HourPlus);
        assert_eq!(grain(None), TimeGrain::Minute);
    }

    /// Carried Minor from A4's review: `median_bar_secs`'s `filter(|d| *d > 0)` branch — a
    /// zero delta (two bars sharing one `ot`) leaves no positive deltas at all, and a negative
    /// (out-of-order) delta mixed among positive ones is dropped rather than counted.
    #[test]
    fn median_bar_secs_filters_non_positive_deltas() {
        // two bars sharing the same ot → delta 0 → filtered out → no deltas left → None.
        let same_ot = vec![bar_at(0, 1_000), bar_at(1, 1_000)];
        assert_eq!(median_bar_secs(&same_ot), None);

        // deltas across the tail: +60_000, -30_000 (out-of-order), +60_000 — only the two
        // positive deltas are counted, so the median is 60_000ms == 60.0s, not skewed by the
        // negative one.
        let mixed = vec![bar_at(0, 0), bar_at(1, 60_000), bar_at(2, 30_000), bar_at(3, 90_000)];
        assert_eq!(median_bar_secs(&mixed), Some(60.0));
    }

    #[test]
    fn mark_step_is_median_gap() {
        assert_eq!(mark_step(&[10.0, 70.0, 130.0, 190.0], 60.0), 60.0);
        assert_eq!(mark_step(&[5.0], 60.0), 60.0); // <2 marks → fallback
        assert_eq!(mark_step(&[], 1440.0), 1440.0);
        // irregular (tick bars): 3,5,100 gaps → median 5
        assert_eq!(mark_step(&[0.0, 3.0, 8.0, 108.0], 60.0), 5.0);
    }

    #[test]
    fn refresh_caches_folds_closed_only_and_invalidates_on_close() {
        let mut cs = ChartState {
            bars: (0..10).map(|i| bar(i, 10.0 - i as f64 * 0.1, 20.0 + i as f64 * 0.1)).collect(),
            closed_len: 9, // last bar is forming
            ..Default::default()
        };
        cs.refresh_caches();
        let (lo, hi) = cs.y_ext.expect("closed bars present");
        assert_eq!(lo, 10.0 - 8.0 * 0.1); // min low over bars 0..=8 (forming excluded)
        assert_eq!(hi, 20.0 + 8.0 * 0.1);

        // forming-bar mutation → same key → no recompute (y_ext unchanged even
        // though the forming bar now has a wilder range)
        cs.bars[9].l = 0.0;
        cs.bars[9].h = 99.0;
        cs.refresh_caches();
        assert_eq!(cs.y_ext, Some((lo, hi)));

        // bar close (closed_len grows) → cache invalidates and picks up bar 9
        cs.closed_len = 10;
        cs.refresh_caches();
        assert_eq!(cs.y_ext, Some((0.0, 99.0)));
    }

    #[test]
    fn refresh_caches_empty_series() {
        let mut cs = ChartState::default();
        cs.refresh_caches();
        assert_eq!(cs.y_ext, None);
        assert!(cs.hour_marks.is_empty());
    }

    /// Oracle: a fresh full min/max fold + `hour_mark_indices`/`day_mark_indices` (in `tz`) +
    /// `median_bar_secs`/`mark_step` (task A4) over the entire closed prefix — exactly what
    /// today's `refresh_caches` computes on every key change. `refresh_caches`'s incremental
    /// output must equal this byte-for-byte after every call, no matter which internal branch
    /// (append fast-path vs. full recompute) it took.
    #[allow(clippy::type_complexity)]
    fn full_cache_recompute(
        closed: &[Bar],
        tz: DisplayTz,
    ) -> (Option<(f64, f64)>, Vec<f64>, Vec<f64>, Option<f64>, f64, f64) {
        let y_ext = if closed.is_empty() {
            None
        } else {
            let lo = closed.iter().map(|b| b.l).fold(f64::INFINITY, f64::min);
            let hi = closed.iter().map(|b| b.h).fold(f64::NEG_INFINITY, f64::max);
            Some((lo, hi))
        };
        let hour_marks = hour_mark_indices(closed, tz);
        let day_marks = day_mark_indices(closed, tz);
        let median_secs = median_bar_secs(closed);
        let hour_step = mark_step(&hour_marks, 60.0);
        let day_step = mark_step(&day_marks, 1440.0);
        (y_ext, hour_marks, day_marks, median_secs, hour_step, day_step)
    }

    /// Assert `cs.y_ext`/`cs.hour_marks`/`cs.day_marks`/`cs.median_secs()`/`cs.hour_step()`/
    /// `cs.day_step()` (as left by the last `refresh_caches` call) equal a fresh full recompute
    /// over `closed` at `cs`'s current tz — the equivalence gate for every step of a tick
    /// sequence, now locking the whole A4 cache family (not just `y_ext`/`hour_marks`) to the
    /// same oracle.
    fn assert_refresh_matches_full(cs: &ChartState, closed: &[Bar], label: &str) {
        let (y_ext, hour_marks, day_marks, median_secs, hour_step, day_step) =
            full_cache_recompute(closed, cs.tz());
        assert_eq!(cs.y_ext, y_ext, "{label}: y_ext diverged from full recompute");
        assert_eq!(cs.hour_marks, hour_marks, "{label}: hour_marks diverged from full recompute");
        assert_eq!(cs.day_marks, day_marks, "{label}: day_marks diverged from full recompute");
        assert_eq!(
            cs.median_secs(),
            median_secs,
            "{label}: median_secs diverged from full recompute"
        );
        assert_eq!(cs.hour_step(), hour_step, "{label}: hour_step diverged from full recompute");
        assert_eq!(cs.day_step(), day_step, "{label}: day_step diverged from full recompute");
    }

    /// `refresh_caches` must be byte-identical to a full recompute at every step of a
    /// realistic tick sequence — seed 120 closed + forming, three forming-only ticks, a
    /// single bar close, a forming tick after that close, five bars closing at once, then
    /// enough further closes to push the closed prefix from 120 to 190 (a ≥60-bar append
    /// span, which — since hour marks recur every 60 one-minute bars for ANY fixed-offset
    /// local timezone — is guaranteed to cross a real hour boundary regardless of the
    /// test machine's timezone), and finally a reload onto a shorter, differently-epoched
    /// series (symbol/timeframe swap). This exercises the append fast-path's `+ prev_len`
    /// hour-mark offset against a genuine mark in the appended region, not just an empty
    /// slice — a wrong offset would silently pass if no real mark ever landed there.
    #[test]
    fn refresh_caches_incremental_matches_full() {
        let mut cs = ChartState::default();

        // seed: 120 closed bars (indices 0..119) + forming (index 120)
        let mut series: Vec<Bar> =
            (0..=120).map(|i| bar(i, 10.0 - i as f64 * 0.01, 20.0 + i as f64 * 0.01)).collect();
        cs.bars = series.clone();
        cs.closed_len = 120;
        cs.refresh_caches();
        assert_refresh_matches_full(&cs, &series[..120], "seed");

        // forming-tick x3: mutate the forming bar (index 120) only — closed prefix stable
        for k in 0..3 {
            series[120].h = 30.0 + k as f64;
            series[120].l = 5.0 - k as f64;
            cs.bars = series.clone();
            cs.refresh_caches();
            assert_refresh_matches_full(&cs, &series[..120], "forming-tick");
        }

        // close 1: bar 120 closes, new forming bar 121
        series.push(bar(121, 5.0, 25.0));
        cs.bars = series.clone();
        cs.closed_len = 121;
        cs.refresh_caches();
        assert_refresh_matches_full(&cs, &series[..121], "close 1");

        // forming-tick after close 1
        series[121].h = 40.0;
        cs.bars = series.clone();
        cs.refresh_caches();
        assert_refresh_matches_full(&cs, &series[..121], "forming-tick after close 1");

        // close 5 at once: bars 121..=125 close, new forming bar 126
        for i in 122..=126 {
            series.push(bar(i, 1.0 + i as f64 * 0.02, 50.0 - i as f64 * 0.02));
        }
        cs.bars = series.clone();
        cs.closed_len = 126;
        cs.refresh_caches();
        assert_refresh_matches_full(&cs, &series[..126], "close 5");

        // further closes pushing the closed prefix 126 -> 190: combined with the closes
        // above, the appended span from the seed boundary (120) to 190 is 70 bars — >60,
        // so it's guaranteed to contain a genuine hour-mark boundary.
        for i in 127..=190 {
            series.push(bar(i, 2.0, 60.0));
        }
        cs.bars = series.clone();
        cs.closed_len = 190;
        cs.refresh_caches();
        assert_refresh_matches_full(&cs, &series[..190], "close to 190");
        assert!(
            cs.hour_marks.iter().any(|&m| m >= 120.0),
            "expected a genuine hour-mark boundary within the appended region (index >= \
             120) so the append fast-path's + prev_len offset is exercised for real, not \
             just against an empty slice"
        );

        // reload onto a shorter, differently-epoched series (symbol/timeframe swap) —
        // forces the full-recompute fallback (boundary mismatch)
        let reload: Vec<Bar> = (0..40)
            .map(|i| {
                let mut b = bar(i, 3.0, 33.0);
                b.ot += 3_600_000; // different epoch boundary -> forces full recompute
                b
            })
            .collect();
        cs.bars = reload.clone();
        cs.closed_len = 40;
        cs.refresh_caches();
        assert_refresh_matches_full(&cs, &reload[..40], "reload");
    }

    /// `refresh_caches` does O(delta) work, not O(history): `last_refresh_work.recomputed`
    /// must read 0 on an unchanged-key call (forming tick), the exact append delta on a
    /// bar close (1, then 5 at once), and the full closed length again on a structural
    /// reload — never more than the bars that actually changed.
    #[test]
    fn refresh_caches_does_delta_work_only() {
        let mut cs = ChartState::default();

        let mut series: Vec<Bar> = (0..=100).map(|i| bar(i, 10.0, 20.0)).collect();
        cs.bars = series.clone();
        cs.closed_len = 100;
        cs.refresh_caches();
        assert_eq!(cs.last_refresh_work.recomputed, 100, "seed is a full recompute");

        // forming-tick: mutate the forming bar (index 100), closed prefix unchanged
        series[100].h = 999.0;
        cs.bars = series.clone();
        cs.refresh_caches();
        assert_eq!(cs.last_refresh_work.recomputed, 0, "forming tick touches no closed bars");

        // close 1
        series.push(bar(101, 5.0, 6.0));
        cs.bars = series.clone();
        cs.closed_len = 101;
        cs.refresh_caches();
        assert_eq!(cs.last_refresh_work.recomputed, 1, "one bar closed");

        // close 5 at once
        for i in 102..=106 {
            series.push(bar(i, 1.0, 2.0));
        }
        cs.bars = series.clone();
        cs.closed_len = 106;
        cs.refresh_caches();
        assert_eq!(cs.last_refresh_work.recomputed, 5, "five bars closed at once");

        // reload onto a shorter, differently-epoched series
        let reload: Vec<Bar> = (0..40)
            .map(|i| {
                let mut b = bar(i, 3.0, 33.0);
                b.ot += 3_600_000;
                b
            })
            .collect();
        cs.bars = reload.clone();
        cs.closed_len = 40;
        cs.refresh_caches();
        assert_eq!(cs.last_refresh_work.recomputed, 40, "reload is a full recompute");
    }

    /// One `vike_model::Bar` (the wire type `sync` reads FROM) at index `i`, on the
    /// same one-minute-bar epoch as [`bar`] above. `jitter` perturbs `close`/`high` so
    /// a "forming tick" mutation is distinguishable from the bar's prior values.
    fn raw_bar(i: usize, jitter: f64) -> vike_model::Bar {
        let ts = 1_700_000_040_000 + i as i64 * 60_000;
        let base = 100.0 + (i as f64 * 0.31).sin() * 3.0;
        vike_model::Bar {
            ts,
            open: base,
            high: base + 1.0 + jitter.abs(),
            low: base - 1.0,
            close: base + jitter,
            volume: 500.0 + i as f64,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// A `vike_model::Bar` (wire type) at an explicit open-time `ts` (ms) — for tz-mark tests
    /// that need specific wall-clock instants rather than [`raw_bar`]'s index-derived
    /// one-minute epoch.
    fn wire_bar_at(ts: i64) -> vike_model::Bar {
        vike_model::Bar {
            ts,
            open: 1.0,
            high: 2.0,
            low: 1.0,
            close: 1.5,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// Oracle: a fresh full clear+rebuild of `(closed, forming)` via `render_bar` —
    /// exactly what today's `sync_from_core` loop (and `sync`'s own full-rebuild
    /// branch) produces. `sync`'s incremental output must equal this byte-for-byte
    /// after every call, no matter which internal branch it took.
    fn full_rebuild(closed: &[vike_model::Bar], forming: Option<&vike_model::Bar>) -> Vec<Bar> {
        let mut bars: Vec<Bar> = closed.iter().enumerate().map(|(i, b)| render_bar(i, b)).collect();
        if let Some(f) = forming {
            let idx = bars.len();
            bars.push(render_bar(idx, f));
        }
        bars
    }

    /// `sync` then assert `cs.bars` equals a fresh full rebuild of the same
    /// `(closed, forming)` — the equivalence gate for every step of a tick sequence.
    fn sync_and_check_equivalence(
        cs: &mut ChartState,
        closed: &[vike_model::Bar],
        forming: Option<&vike_model::Bar>,
        label: &str,
    ) {
        cs.sync(&std::sync::Arc::new(closed.to_vec()), forming);
        assert_eq!(
            cs.bars,
            full_rebuild(closed, forming),
            "{label}: bars diverged from full rebuild"
        );
    }

    /// `sync` must be byte-identical to a full clear+rebuild at every step of a
    /// realistic tick sequence: seed, five forming-only ticks, a single bar close, a
    /// forming tick after that close, three bars closing at once (GUI polled less
    /// often than bars closed), then a reload onto a shorter, differently-shaped
    /// series (symbol/timeframe swap). Exercises all three `sync` branches
    /// (unchanged-prefix, append, full-rebuild) against the same oracle.
    #[test]
    fn sync_matches_full_rebuild_over_tick_sequence() {
        let mut cs = ChartState::default();
        let mut closed: Vec<vike_model::Bar> = (0..100).map(|i| raw_bar(i, 0.3)).collect();
        let mut forming = raw_bar(100, 0.0);

        sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "seed");

        for k in 0..5 {
            forming.close += (k + 1) as f64 * 0.02;
            forming.high = forming.high.max(forming.close);
            sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "forming-tick");
        }

        closed.push(forming); // bar 100 closes
        forming = raw_bar(101, -0.1);
        sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "close 1");

        forming.close -= 0.03;
        sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "forming-tick after close 1");

        closed.push(forming); // bar 101 closes
        closed.push(raw_bar(102, 0.15)); // + two more that closed while unpolled
        closed.push(raw_bar(103, -0.25));
        forming = raw_bar(104, 0.0);
        sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "close 3");

        let reload_closed: Vec<vike_model::Bar> = (0..40).map(|i| raw_bar(i, -0.5)).collect();
        let reload_forming = raw_bar(40, 0.2);
        sync_and_check_equivalence(&mut cs, &reload_closed, Some(&reload_forming), "reload");
    }

    /// `sync` does O(delta) rendering work, not O(history): the work counter must
    /// read 0 on an unchanged-prefix forming tick, the exact delta on an append (1
    /// bar, then 3 at once), and the full length again on a structural reload — never
    /// more than the bars that actually changed.
    #[test]
    fn sync_does_delta_work_only() {
        let mut cs = ChartState::default();
        let mut closed: Vec<vike_model::Bar> = (0..100).map(|i| raw_bar(i, 0.3)).collect();
        let mut forming = raw_bar(100, 0.0);

        cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
        assert_eq!(cs.last_sync_work.closed_rerendered, 100, "seed renders every closed bar");
        assert!(cs.last_sync_work.forming_rerendered);

        forming.close += 0.05;
        cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
        assert_eq!(cs.last_sync_work.closed_rerendered, 0, "forming tick touches no closed bars");

        closed.push(forming); // bar 100 closes
        forming = raw_bar(101, -0.1);
        cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
        assert_eq!(cs.last_sync_work.closed_rerendered, 1, "one bar closed");

        closed.push(forming); // bar 101 closes
        closed.push(raw_bar(102, 0.15));
        closed.push(raw_bar(103, -0.25));
        forming = raw_bar(104, 0.0);
        cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
        assert_eq!(cs.last_sync_work.closed_rerendered, 3, "three bars closed at once");

        let reload_closed: Vec<vike_model::Bar> = (0..40).map(|i| raw_bar(i, -0.5)).collect();
        cs.sync(&std::sync::Arc::new(reload_closed.clone()), None);
        assert_eq!(cs.last_sync_work.closed_rerendered, 40, "reload is a full rebuild");
        assert!(!cs.last_sync_work.forming_rerendered);
    }

    /// `set_tz` must invalidate `hour_marks` on the very next `sync`, even though the closed
    /// bars themselves are byte-identical (so `sync`'s own bars-content dedup takes the
    /// "unchanged prefix" fast path) — `refresh_caches`'s `cache_key` carries the tz identity
    /// specifically so this doesn't go stale.
    #[test]
    fn set_tz_invalidates_mark_cache() {
        let base = chrono::DateTime::parse_from_rfc3339("2026-07-11T11:30:00Z")
            .unwrap()
            .timestamp_millis();
        let closed: Vec<vike_model::Bar> =
            (0..4).map(|i| wire_bar_at(base + i as i64 * 900_000)).collect();
        let mut cs = ChartState::default();
        cs.set_tz(DisplayTz::Utc);
        cs.sync(&std::sync::Arc::new(closed.clone()), None);
        assert_eq!(cs.hour_marks, vec![2.0]);
        cs.set_tz(DisplayTz::Named(chrono_tz::Asia::Kolkata));
        cs.sync(&std::sync::Arc::new(closed.clone()), None); // same bars, new tz → cache must refresh
        assert_eq!(cs.hour_marks, vec![0.0]);
    }

    /// [`ChartState::visible_vol_max`] must cache by `(lo, hi, cache_key)`: a repeat call
    /// with the same window is an O(1) cache hit (no work-counter bump); a different
    /// window, or a bar CLOSE (which bumps `cache_key`), forces a genuine recompute. Every
    /// returned value must also equal a plain fold over the CLOSED window
    /// `bars[lo..hi.min(closed_len)]` — the oracle this cache must never diverge from.
    #[test]
    fn visible_vol_max_caches_by_key() {
        // 100 closed bars, v = 1.0..=100.0 (bar i has v = i + 1).
        let mut cs = ChartState {
            bars: (0..100)
                .map(|i| {
                    let mut b = bar(i, 10.0, 20.0);
                    b.v = (i + 1) as f64;
                    b
                })
                .collect(),
            closed_len: 100,
            ..Default::default()
        };
        cs.refresh_caches(); // seeds cache_key

        // oracle: a plain fold over the CLOSED window, matching visible_vol_max's contract.
        let oracle = |cs: &ChartState, lo: usize, hi: usize| -> f64 {
            let hi = hi.min(cs.closed_len);
            let lo = lo.min(hi);
            cs.bars[lo..hi].iter().map(|b| b.v).fold(0.0_f64, f64::max)
        };

        assert_eq!(cs.visible_vol_max(0, 50), 50.0, "max of v=1..=50 is 50.0");
        assert_eq!(cs.visible_vol_max(0, 50), oracle(&cs, 0, 50));
        assert_eq!(cs.vol_recompute_count.get(), 1, "first call is a genuine miss");

        // same (lo, hi), same cache_key -> cache hit, no recompute.
        assert_eq!(cs.visible_vol_max(0, 50), 50.0);
        assert_eq!(cs.vol_recompute_count.get(), 1, "repeat call must be a cache hit");

        // different window -> recompute (count bumps).
        assert_eq!(cs.visible_vol_max(10, 60), oracle(&cs, 10, 60));
        assert_eq!(cs.vol_recompute_count.get(), 2, "a different (lo, hi) must miss");

        // repeat the NEW window -> cache hit again.
        assert_eq!(cs.visible_vol_max(10, 60), oracle(&cs, 10, 60));
        assert_eq!(cs.vol_recompute_count.get(), 2, "repeat of the new window must hit");

        // close a bar (bumps cache_key via refresh_caches) -> same (lo, hi) now misses.
        let mut closing = bar(100, 10.0, 20.0);
        closing.v = 999.0; // outside [10, 60), so the oracle value is unchanged...
        cs.bars.push(closing);
        cs.closed_len = 101;
        cs.refresh_caches(); // bumps cache_key (closed_len 100 -> 101)
        assert_eq!(
            cs.visible_vol_max(10, 60),
            oracle(&cs, 10, 60),
            "value is unchanged (bar 100 is outside [10,60)), but the key changed"
        );
        assert_eq!(
            cs.vol_recompute_count.get(),
            3,
            "a bar close must force a recompute even for the same window"
        );

        // empty window -> 0.0 (the fold's zero identity), and does not panic.
        assert_eq!(cs.visible_vol_max(60, 10), 0.0, "lo > hi collapses to an empty window");
        assert_eq!(cs.visible_vol_max(200, 300), 0.0, "window entirely past closed_len");
    }

    /// [`ChartState::visible_vol_max`] only ever folds the CLOSED prefix — a forming bar
    /// (`bars[closed_len..]`) sitting inside `[lo, hi)` must NOT be picked up, even though
    /// today's inline `visible_slice(bars, ..)` fold (which callers replace with this cache
    /// PLUS their own `.max(forming.v)`) would see it as just the last element. This is the
    /// forming-visible/forming-hidden equivalence the two call sites depend on.
    #[test]
    fn visible_vol_max_excludes_the_forming_bar() {
        let mut cs = ChartState {
            bars: (0..10)
                .map(|i| {
                    let mut b = bar(i, 10.0, 20.0);
                    b.v = 5.0; // closed bars: uniform v=5.0
                    b
                })
                .collect(),
            closed_len: 9, // bar index 9 is forming
            ..Default::default()
        };
        cs.bars[9].v = 1_000.0; // forming bar: wildly larger volume
        cs.refresh_caches();

        // forming bar (index 9) IS inside [0, 10) -> must still be excluded from the CLOSED-only max.
        assert_eq!(
            cs.visible_vol_max(0, 10),
            5.0,
            "forming bar's huge v must not leak into the closed-only max"
        );

        // sanity: the SAME window over the full `bars` slice (closed + forming), i.e. what
        // today's inline fold computes, DOES pick up the forming bar — proving the two are
        // deliberately different, and that callers must .max() the forming value in themselves.
        let full_fold = cs.bars[0..10].iter().map(|b| b.v).fold(0.0_f64, f64::max);
        assert_eq!(full_fold, 1_000.0);
    }

    /// `visible_vol_max` returns [`ChartState::visible_vol_max_shared`]'s value exactly —
    /// the `&mut self` entry point is a thin forwarder, not a second cache/implementation.
    #[test]
    fn visible_vol_max_matches_shared_twin() {
        let mut cs = ChartState {
            bars: (0..20)
                .map(|i| {
                    let mut b = bar(i, 10.0, 20.0);
                    b.v = (i * 3) as f64;
                    b
                })
                .collect(),
            closed_len: 20,
            ..Default::default()
        };
        let oracle = cs.bars[2..15].iter().map(|b| b.v).fold(0.0_f64, f64::max);
        assert_eq!(cs.visible_vol_max_shared(2, 15), oracle);
        assert_eq!(
            cs.visible_vol_max(2, 15),
            oracle,
            "&mut self entry point must agree with the &self twin"
        );
    }

    // === SP2 T4: visible-range volume-profile overlay cache ================================

    fn fp(idx: u64, cells: &[(f64, f64, f64)]) -> FootprintBar {
        FootprintBar {
            bar_index: idx,
            cells: cells
                .iter()
                .map(|&(p, b, s)| vike_orderflow::PriceBin { price: p, buy_vol: b, sell_vol: s })
                .collect(),
        }
    }

    /// [`ChartState::visible_profile`] must cache by [`ProfileCacheKey`]: a repeat call with
    /// the same `(i0, i1, footprints.len(), tick_size)` is an O(1) cache hit (no recompute-
    /// counter bump); a different range, a different footprint length, or a different tick
    /// size each force a genuine recompute. Every returned value must also equal a direct
    /// [`VolumeProfile::from_footprints`] call — the oracle this cache must never diverge from.
    #[test]
    fn visible_profile_caches_by_key() {
        let cs = ChartState::default();
        let fps = vec![
            fp(0, &[(100.0, 5.0, 2.0)]),
            fp(1, &[(100.0, 1.0, 0.0), (101.0, 0.0, 4.0)]),
            fp(2, &[(101.0, 3.0, 0.0)]),
        ];
        let oracle =
            |ts: f64, i0: usize, i1: usize| VolumeProfile::from_footprints(&fps, ts, i0, i1);

        assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 0, 2), oracle(1.0, 0, 2));
        assert_eq!(cs.profile_recompute_count.get(), 1, "first call is a genuine miss");

        // same (i0, i1, len, tick_size) -> cache hit, no recompute.
        assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 0, 2), oracle(1.0, 0, 2));
        assert_eq!(cs.profile_recompute_count.get(), 1, "repeat call must be a cache hit");

        // different range -> miss.
        assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 1, 2), oracle(1.0, 1, 2));
        assert_eq!(cs.profile_recompute_count.get(), 2, "a different (i0, i1) must miss");

        // repeat the new range -> hit.
        assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 1, 2), oracle(1.0, 1, 2));
        assert_eq!(cs.profile_recompute_count.get(), 2, "repeat of the new range must hit");

        // same range, different tick_size -> miss (bucket width changed, len unchanged).
        assert_eq!(*cs.visible_profile_shared(&fps, 2.0, 1, 2), oracle(2.0, 1, 2));
        assert_eq!(cs.profile_recompute_count.get(), 3, "a different tick_size must miss");

        // shorter footprint slice (len changes, same range/tick_size) -> miss. Oracle is over
        // the SAME shortened slice — `from_footprints` clamps `hi` to the slice's own length,
        // so comparing against the full-`fps` oracle would silently check a different
        // computation (a different `hi` clamp) rather than the cache-vs-direct equivalence.
        let short = &fps[..2];
        assert_eq!(
            *cs.visible_profile_shared(short, 2.0, 1, 2),
            VolumeProfile::from_footprints(short, 2.0, 1, 2)
        );
        assert_eq!(cs.profile_recompute_count.get(), 4, "a different footprint length must miss");
    }

    /// `visible_profile` returns [`ChartState::visible_profile_shared`]'s value exactly — the
    /// `&mut self` entry point is a thin forwarder, not a second cache/implementation.
    #[test]
    fn visible_profile_matches_shared_twin() {
        let mut cs = ChartState::default();
        let fps = vec![fp(0, &[(50.0, 2.0, 1.0)]), fp(1, &[(51.0, 0.0, 3.0)])];
        let oracle = VolumeProfile::from_footprints(&fps, 1.0, 0, 1);
        assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 0, 1), oracle);
        assert_eq!(
            *cs.visible_profile(&fps, 1.0, 0, 1),
            oracle,
            "&mut self entry point must agree with the &self twin"
        );
    }

    /// Empty/out-of-range windows must not panic and must match
    /// [`VolumeProfile::from_footprints`]'s own documented empty behavior (poc 0.0, va (0,0),
    /// no bins) — the cache is a pure memoization layer, never a second source of truth for
    /// edge-case handling.
    #[test]
    fn visible_profile_empty_range_is_the_from_footprints_default() {
        let cs = ChartState::default();
        let fps = vec![fp(0, &[(100.0, 5.0, 2.0)])];
        let empty = cs.visible_profile_shared(&fps, 1.0, 5, 9);
        assert!(empty.bins.is_empty());
        assert_eq!(empty.poc, 0.0);
        assert_eq!(empty.value_area, (0.0, 0.0));
        // lo > hi also collapses to the same empty result, matching from_footprints directly.
        assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 2, 1), *empty);
    }

    // === SP3 Task B #2 (SP2 final-review finding B) + SP3 TB-fix (post-Task-B review finding,
    // MEDIUM: generation-hardened against ABA staleness) CVD pane recompute cache ============

    /// [`ChartState::cvd_shared`] must cache by [`CvdCacheKey`] (`(generation, len)`): a repeat
    /// call at the SAME generation+len is an O(1) cache hit — no recompute-counter bump, and the
    /// exact same `Rc` handed back (proving it wasn't just a coincidental content match from an
    /// independent recompute) — while a generation bump is ALWAYS a genuine miss, even over the
    /// byte-identical slice at the SAME address. That last property is the ABA-hardening fix
    /// itself under direct test: the old `(ptr, len)` key would have wrongly served the stale
    /// cached `Rc` here (same address, same length); keying on the caller-supplied generation
    /// instead means a bumped generation is trusted as "content changed" without re-deriving that
    /// from the slice's own (recyclable) address. Every returned value must equal a direct
    /// `orderflow::cvd_from_footprints` call — the oracle this cache must never diverge from.
    #[test]
    fn cvd_shared_caches_by_generation_and_len_not_pointer_identity() {
        let cs = ChartState::default();
        let fps = vec![
            fp(0, &[(100.0, 5.0, 2.0)]),                    // delta +3
            fp(1, &[(101.0, 1.0, 4.0)]),                    // delta -3
            fp(2, &[(100.0, 2.0, 0.0), (101.0, 0.0, 1.0)]), // delta +1
        ];
        let oracle = crate::orderflow::cvd_from_footprints(&fps);

        let first = cs.cvd_shared(&fps, 1);
        assert_eq!(*first, oracle);
        assert_eq!(cs.cvd_recompute_count.get(), 1, "first call is a genuine miss");

        // same slice (same address AND length), same generation -> cache hit: same Rc, no
        // recompute.
        let second = cs.cvd_shared(&fps, 1);
        assert!(
            Rc::ptr_eq(&first, &second),
            "a repeat call at the SAME generation must hand back the SAME Rc"
        );
        assert_eq!(cs.cvd_recompute_count.get(), 1, "repeat call must be a cache hit");

        // the IDENTICAL slice (same address, same length, same content) but a bumped generation
        // -> must MISS. This is the ABA scenario the fix targets: a toggle-off/on cycle can hand
        // `cvd_shared` a slice whose address+length coincidentally match the stale cached key even
        // though the content is logically new; the generation is the caller's authoritative signal
        // that it changed, and must win over the slice's own identity.
        let third = cs.cvd_shared(&fps, 2);
        assert_eq!(*third, oracle);
        assert_eq!(
            cs.cvd_recompute_count.get(),
            2,
            "a generation bump must miss even over the identical slice"
        );

        // a shorter slice of the SAME underlying allocation, SAME generation as `third` -> still
        // a miss (len is a belt-and-suspenders second key component).
        let shorter = &fps[..2];
        let fourth = cs.cvd_shared(shorter, 2);
        assert_eq!(*fourth, crate::orderflow::cvd_from_footprints(shorter));
        assert_eq!(cs.cvd_recompute_count.get(), 3, "a different length must miss");

        // repeat the full slice at generation 1 again -> miss (the slot now holds generation 2's
        // key from the calls above).
        let fifth = cs.cvd_shared(&fps, 1);
        assert_eq!(*fifth, oracle);
        assert_eq!(
            cs.cvd_recompute_count.get(),
            4,
            "the slot was evicted by the later-generation calls"
        );
    }

    /// Empty footprints must not panic and must match `cvd_from_footprints(&[])`'s own
    /// documented empty behavior (an empty `Vec`) — the cache is a pure memoization layer, never
    /// a second source of truth for edge-case handling.
    #[test]
    fn cvd_shared_empty_footprints_is_empty() {
        let cs = ChartState::default();
        assert!(cs.cvd_shared(&[], 0).is_empty());
    }

    // === chart-perf T6: transform-style recompute cache ===================================

    const T6_PARAMS: TransformParams = TransformParams { line_break_n: 3, pnf_reversal: 3 };

    /// A render `Bar` with a ±1 range around `close` — one-minute epoch like [`bar`] above.
    fn tbar(i: usize, close: f64) -> Bar {
        Bar {
            t: i as f64,
            ot: 1_700_000_040_000 + i as i64 * 60_000,
            o: close,
            h: close + 1.0,
            l: close - 1.0,
            c: close,
            v: 100.0 + i as f64,
        }
    }

    /// Trend + oscillation so every transform (Renko/Range/LineBreak/Kagi/PnF) sees real
    /// breakouts against its `auto_box`(~2.0, since h-l == 2) — otherwise the transforms return
    /// empty vecs and the equivalence gate would pass vacuously.
    fn close_at(i: usize) -> f64 {
        100.0 + (i as f64 * 0.05).sin() * 8.0 + i as f64 * 0.03
    }

    /// The oracle: exactly how `chart.rs` builds the `owned` proxy for each transform style over a
    /// full (closed + forming) series — independent of [`ChartState::transformed`]'s internals.
    fn oracle_transform(
        style: ChartStyle,
        params: TransformParams,
        bars: &[Bar],
        first_ot: i64,
    ) -> Vec<Bar> {
        use ChartStyle::*;
        match style {
            HeikinAshi => heikin_ashi(bars),
            Renko => transforms::reindex(transforms::renko(bars)),
            Range => transforms::reindex(transforms::range_bars(bars)),
            LineBreak => transforms::reindex(transforms::line_break(bars, params.line_break_n)),
            Kagi => transforms::reindex(
                transforms::kagi(bars)
                    .prices
                    .iter()
                    .map(|&p| Bar { t: 0.0, ot: first_ot, o: p, h: p, l: p, c: p, v: 0.0 })
                    .collect(),
            ),
            PointFigure => {
                let (cols, _) = transforms::point_and_figure(bars, params.pnf_reversal);
                transforms::reindex(
                    cols.iter()
                        .map(|c| Bar {
                            t: 0.0,
                            ot: first_ot,
                            o: c.bottom,
                            h: c.top,
                            l: c.bottom,
                            c: c.top,
                            v: 0.0,
                        })
                        .collect(),
                )
            }
            _ => vec![],
        }
    }

    /// Byte/value-identical assertion — every f64 field compared by `to_bits`, not `==`.
    fn assert_bars_bit_identical(got: &[Bar], want: &[Bar], label: &str) {
        assert_eq!(got.len(), want.len(), "{label}: length {} != oracle {}", got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert_eq!(g.t.to_bits(), w.t.to_bits(), "{label}[{i}].t");
            assert_eq!(g.ot, w.ot, "{label}[{i}].ot");
            assert_eq!(g.o.to_bits(), w.o.to_bits(), "{label}[{i}].o");
            assert_eq!(g.h.to_bits(), w.h.to_bits(), "{label}[{i}].h");
            assert_eq!(g.l.to_bits(), w.l.to_bits(), "{label}[{i}].l");
            assert_eq!(g.c.to_bits(), w.c.to_bits(), "{label}[{i}].c");
            assert_eq!(g.v.to_bits(), w.v.to_bits(), "{label}[{i}].v");
        }
    }

    fn set_series(cs: &mut ChartState, bars: &[Bar], closed_len: usize) {
        cs.bars = bars.to_vec();
        cs.closed_len = closed_len;
        cs.refresh_caches();
    }

    /// Install `(bars, closed_len)`, run `transformed`, and assert it is byte-identical to the
    /// oracle full-transform of (closed + forming).
    fn drive_and_check(
        cs: &mut ChartState,
        bars: &[Bar],
        closed_len: usize,
        style: ChartStyle,
        label: &str,
    ) {
        set_series(cs, bars, closed_len);
        let got = cs.transformed(style, T6_PARAMS);
        let first_ot = cs.bars.first().map_or(0, |b| b.ot);
        let want = oracle_transform(style, T6_PARAMS, &cs.bars, first_ot);
        assert_bars_bit_identical(&got, &want, label);
    }

    /// THE gate: for EACH of the six transform styles, `transformed` is byte-identical to a full
    /// recompute of (closed + forming) at every step of a realistic tick sequence — seed 300
    /// closed + forming, five forming-only ticks, a bar close, another forming tick, then five
    /// bars closing at once.
    #[test]
    fn transformed_matches_full_recompute_per_style() {
        use ChartStyle::*;
        for style in [HeikinAshi, Renko, Range, LineBreak, Kagi, PointFigure] {
            let mut cs = ChartState::default();

            // seed: 300 closed bars (0..299) + forming (300)
            let mut series: Vec<Bar> = (0..=300).map(|i| tbar(i, close_at(i))).collect();
            drive_and_check(&mut cs, &series, 300, style, &format!("{style:?} seed"));
            assert!(
                !cs.transformed(style, T6_PARAMS).is_empty(),
                "{style:?}: expected a non-empty transform (the gate must not pass vacuously)"
            );

            // forming-tick x5: mutate the forming bar (index 300) only
            for k in 0..5 {
                series[300] = tbar(300, close_at(300) + (k as f64 + 1.0) * 0.7);
                drive_and_check(
                    &mut cs,
                    &series,
                    300,
                    style,
                    &format!("{style:?} forming-tick {k}"),
                );
            }

            // close 1: bar 300 closes, new forming 301
            series.push(tbar(301, close_at(301)));
            drive_and_check(&mut cs, &series, 301, style, &format!("{style:?} close 1"));

            // forming-tick after close 1
            series[301] = tbar(301, close_at(301) + 1.3);
            drive_and_check(
                &mut cs,
                &series,
                301,
                style,
                &format!("{style:?} forming-tick after close 1"),
            );

            // close 5 at once: bars 302..=306 append, closed prefix 301 -> 306, forming 306
            for i in 302..=306 {
                series.push(tbar(i, close_at(i)));
            }
            drive_and_check(&mut cs, &series, 306, style, &format!("{style:?} close 5"));
        }
    }

    /// The work counter: closed-prefix recomputes bump only on bar-close / style / param change
    /// for the TWO-TIER styles (HeikinAshi/LineBreak) and never on a forming tick or a static
    /// frame; the FALLBACK styles (Renko/Range/Kagi/PnF) serve a static frame from cache (no bump)
    /// but reprocess on a forming tick (their `auto_box` reads the forming bar) — the documented
    /// residual cost.
    #[test]
    fn transformed_recomputes_only_tail() {
        use ChartStyle::*;

        // two-tier: forming ticks reuse the cached closed prefix.
        for style in [HeikinAshi, LineBreak] {
            let mut cs = ChartState::default();
            let mut series: Vec<Bar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
            set_series(&mut cs, &series, 200);
            cs.transformed(style, T6_PARAMS);
            assert_eq!(
                cs.transform_closed_recompute_count.get(),
                1,
                "{style:?}: seed builds the closed prefix once"
            );

            // static repeat (identical state) → O(1) cache hit, no bump
            cs.transformed(style, T6_PARAMS);
            assert_eq!(
                cs.transform_closed_recompute_count.get(),
                1,
                "{style:?}: static repeat is a cache hit"
            );

            // forming ticks → closed prefix unchanged → reuse, no bump
            for k in 0..4 {
                series[200] = tbar(200, close_at(200) + (k as f64 + 1.0) * 0.6);
                set_series(&mut cs, &series, 200);
                cs.transformed(style, T6_PARAMS);
            }
            assert_eq!(
                cs.transform_closed_recompute_count.get(),
                1,
                "{style:?}: forming ticks reuse the closed prefix"
            );

            // bar close → closed prefix changes → bump
            series.push(tbar(201, close_at(201)));
            set_series(&mut cs, &series, 201);
            cs.transformed(style, T6_PARAMS);
            assert_eq!(
                cs.transform_closed_recompute_count.get(),
                2,
                "{style:?}: a bar close rebuilds the closed prefix"
            );

            // param change → different key → bump
            cs.transformed(style, TransformParams { line_break_n: 2, pnf_reversal: 3 });
            assert_eq!(
                cs.transform_closed_recompute_count.get(),
                3,
                "{style:?}: a param change rebuilds the closed prefix"
            );

            // style change → different key → bump
            cs.transformed(Candles, T6_PARAMS); // non-transform style: still a key change
            assert_eq!(
                cs.transform_closed_recompute_count.get(),
                4,
                "{style:?}: a style change rebuilds"
            );
        }

        // fallback: static frame cached, forming tick reprocesses.
        for style in [Renko, Range, Kagi, PointFigure] {
            let mut cs = ChartState::default();
            let mut series: Vec<Bar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
            set_series(&mut cs, &series, 200);
            cs.transformed(style, T6_PARAMS);
            assert_eq!(cs.transform_closed_recompute_count.get(), 1, "{style:?}: seed recompute");

            // static repeat → cache hit, no bump (the win the fallback still delivers)
            cs.transformed(style, T6_PARAMS);
            assert_eq!(
                cs.transform_closed_recompute_count.get(),
                1,
                "{style:?}: static repeat is a cache hit"
            );

            // forming tick → global auto_box shifts → must reprocess → bump
            series[200] = tbar(200, close_at(200) + 3.0);
            set_series(&mut cs, &series, 200);
            cs.transformed(style, T6_PARAMS);
            assert_eq!(
                cs.transform_closed_recompute_count.get(),
                2,
                "{style:?}: a forming tick reprocesses (global auto_box)"
            );
        }
    }

    /// The Kagi/PnF structured results the custom drawing reads (`cached_{kagi,pnf}_shared`) are
    /// populated by `transformed` and byte-identical to a fresh `transforms::{kagi,point_and_figure}`
    /// over (closed + forming) — the data-flow the `chart.rs` `draw_kagi`/`draw_pnf` rewire relies
    /// on. Each style leaves the OTHER structured slot empty.
    #[test]
    fn cached_structured_matches_fresh_compute() {
        let mut cs = ChartState::default();
        let series: Vec<Bar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
        set_series(&mut cs, &series, 200);

        cs.transformed(ChartStyle::Kagi, T6_PARAMS);
        let k = cs.cached_kagi_shared().expect("Kagi style populates the structured Kagi cache");
        let fresh = transforms::kagi(&cs.bars);
        assert_eq!(k.prices.len(), fresh.prices.len(), "kagi price count");
        for (a, b) in k.prices.iter().zip(&fresh.prices) {
            assert_eq!(a.to_bits(), b.to_bits(), "kagi price bits");
        }
        assert_eq!(k.thick, fresh.thick, "kagi thick flags");
        assert!(cs.cached_pnf_shared().is_none(), "Kagi style leaves the PnF slot empty");

        cs.transformed(ChartStyle::PointFigure, T6_PARAMS);
        let pnf = cs.cached_pnf_shared().expect("PnF style populates the structured PnF cache");
        let (cols, box_) = transforms::point_and_figure(&cs.bars, T6_PARAMS.pnf_reversal);
        assert_eq!(pnf.1.to_bits(), box_.to_bits(), "pnf box bits");
        assert_eq!(pnf.0.len(), cols.len(), "pnf column count");
        for (a, b) in pnf.0.iter().zip(&cols) {
            assert_eq!(a.up, b.up, "pnf column up flag");
            assert_eq!(a.top.to_bits(), b.top.to_bits(), "pnf column top bits");
            assert_eq!(a.bottom.to_bits(), b.bottom.to_bits(), "pnf column bottom bits");
        }
        assert!(cs.cached_kagi_shared().is_none(), "PnF style leaves the Kagi slot empty");
    }
}
