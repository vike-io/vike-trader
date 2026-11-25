//! A chart's history BY ITS VIEW: which window, at which bar width, a chart should hold for what it
//! shows, and the session's cache of what the datahub already answered (`plan_store_reads`' half that
//! remembers, and the read thread's half that fills it).
//!
//! # The rule
//!
//! A chart asks for the bars it SHOWS, never a fixed depth: at most `min(MAX_VIEW_CANDLES, the plot's
//! width in points)` candles, at the FINEST bar width that keeps the view within that (the chart's own
//! interval, else the first `vike_marketdata::interval::KLINE_MENU` width that is a whole multiple of it,
//! which the datahub resamples to). So a `1m` chart zoomed out over three years holds about 1,096 daily
//! candles, and zoomed into one day holds that day's 1,440 minutes. It asks again when the view moves or
//! zooms, once the view has held still for `VIEW_DEBOUNCE_MS`, and only for the pieces of the window the
//! cache does not hold.
//!
//! # What the chart holds is exactly the window planned
//!
//! The chart's x axis is a BAR INDEX, so a chart whose bars are replaced must be re-framed, or the old
//! index range would show other times. The display is therefore always exactly the window last planned
//! (cut from the cache), and the frame that first draws it calls the chart's own seed reset
//! (`ChartFollowLive::on_series_change`, through `ChartHistory::reframe`), which frames the whole
//! display: the window the view asked for. A view within `VIEW_SLACK_BARS` of that display IS that
//! display (the reset shows one empty bar each side), so landing never plans again.
//!
//! # A series the node publishes
//!
//! Its tail paints from the snapshot, so only a view panned left of the oldest bar the chart holds
//! asks, at the chart's own interval, for the bars from the view's left edge to that bar
//! (`ChartHistory::plan_under_live_tail`, on any datahub). They land UNDER the tail rather than in
//! place of it, and the chart re-frames by itself on the jump in its bar count.
//!
//! # An older datahub
//!
//! One without `bar_resample` is asked as before this module: the last `preferences.chart_history_bars`
//! bars of the chart's interval, once per session, and nothing on zoom or scroll (`ChartHistoryCache`'s
//! `resamples`, learned by the read thread from the server's `Welcome`).
//!
//! Not in scope: a cache on disk across restarts (each launch asks again), and live bars into a
//! stored chart (the window holding `now` is re-asked whenever a view that holds it is planned again).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use vike_chart::ChartFollowLive;
use vike_chart::chart::ChartViewSpan;
use vike_chart::chart::state::bar::ChartBar;
use vike_marketdata::Bar;
use vike_marketdata::interval::{KLINE_MENU, bar_open, bar_step_ms};

use crate::data::data_sink::STORED_BAR_CLOSED_CAP;

/// The most candles a view asks for, whatever its width: the owner's figure (2026-11-18), about what a
/// wide monitor can tell apart.
pub const MAX_VIEW_CANDLES: i64 = 2_000;

/// How long a view must hold still before it is asked for: a zoom or a drag moves it every frame.
pub const VIEW_DEBOUNCE_MS: i64 = 300;

/// How many bars a view may stray past the display it was given and still BE that display: the chart's
/// Reset view shows one empty bar each side (`fx_min = -1`, `fx_max = last + 1`) and the plot settles
/// within a bar of it.
pub const VIEW_SLACK_BARS: i64 = 3;

/// The most bars the session's cache keeps (about 30 MB): past it, the series and width used longest
/// ago is dropped whole, and a view of it is asked again.
pub const CACHE_BARS_CEILING: usize = 250_000;

const _: () = assert!(
    MAX_VIEW_CANDLES + 2 * VIEW_SLACK_BARS < STORED_BAR_CLOSED_CAP as i64
        && STORED_BAR_CLOSED_CAP <= vike_datahub_client::proto::RESAMPLED_BARS_CEILING,
    "a window is clamped to what the display store holds, which must be under what one resampled reply carries"
);

/// One window of a chart's history at one bar width: every bar of width `step_ms` that opens in
/// `[start, end]` (epoch ms, inclusive; `start` is a bar open, `end` the last millisecond of a bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChartHistoryWindow {
    pub step_ms: i64,
    pub start: i64,
    pub end: i64,
}

impl ChartHistoryWindow {
    /// The window of bars of `step_ms` from the one holding `first` to the one holding `last`, the newest
    /// no later than the one holding `now_ms`, and at most `STORED_BAR_CLOSED_CAP` bars (the newest kept).
    fn over(step_ms: i64, first: i64, last: i64, now_ms: i64) -> Self {
        let end_open = bar_open(last, step_ms).min(bar_open(now_ms, step_ms));
        let start_open = bar_open(first, step_ms)
            .min(end_open)
            .max(end_open - (STORED_BAR_CLOSED_CAP as i64 - 1) * step_ms);
        Self { step_ms, start: start_open, end: end_open + step_ms - 1 }
    }

    /// How many bars of its width it spans.
    pub fn bars(&self) -> i64 {
        (self.end + 1 - self.start) / self.step_ms
    }
}

/// The widths a chart of `native` ms may be shown at, finest first, each with the interval the datahub is
/// asked to resample to (`None` for the chart's own).
fn ladder(native: i64) -> impl Iterator<Item = (i64, Option<&'static str>)> {
    std::iter::once((native, None)).chain(KLINE_MENU.iter().filter_map(move |to| {
        let step = bar_step_ms(to)?;
        (step > native && step % native == 0).then_some((step, Some(*to)))
    }))
}

/// The most candles a view of `width_px` asks for.
fn budget(width_px: f32) -> i64 {
    if width_px >= 1.0 { (width_px.round() as i64).min(MAX_VIEW_CANDLES) } else { MAX_VIEW_CANDLES }
}

/// The window a view of `(t0, t1)` asks for on a chart of `native` ms: the finest width of the ladder that
/// keeps it within the budget (the coarsest when none does), and the interval to resample to.
pub(crate) fn view_window(
    native: i64,
    (t0, t1): (i64, i64),
    width_px: f32,
    now_ms: i64,
) -> (ChartHistoryWindow, Option<&'static str>) {
    let (t0, t1) = (t0.min(t1), t0.max(t1));
    let most = budget(width_px) + 2 * VIEW_SLACK_BARS;
    let mut chosen = (native, None);
    for (step, to) in ladder(native) {
        chosen = (step, to);
        if (t1 - t0) / step <= most {
            break;
        }
    }
    (ChartHistoryWindow::over(chosen.0, t0, t1, now_ms), chosen.1)
}

/// The window an EMPTY chart of `native` ms asks for first: the newest `budget` bars of its own interval.
pub(crate) fn opening_window(
    native: i64,
    width_px: Option<f32>,
    now_ms: i64,
) -> ChartHistoryWindow {
    let bars = budget(width_px.unwrap_or(0.0));
    ChartHistoryWindow::over(native, now_ms - (bars - 1) * native, now_ms, now_ms)
}

/// Whether a view of `(t0, t1)` asking for bars of `step_ms` is what `shown` already displays.
fn displays(shown: &ChartHistoryWindow, step_ms: i64, (t0, t1): (i64, i64)) -> bool {
    let slack = VIEW_SLACK_BARS * step_ms;
    shown.step_ms == step_ms && t0.min(t1) >= shown.start - slack && t0.max(t1) <= shown.end + slack
}

/// The frame thread's half: what each chart window shows and since when, and the window each chart was
/// last given. [`ChartHistory::cache`] is the half the read thread shares.
#[derive(Default)]
pub struct ChartHistory {
    views: HashMap<egui::Id, (ChartViewSpan, i64)>,
    shown: HashMap<String, ChartHistoryWindow>,
    cache: Arc<ChartHistoryCache>,
}

/// What a view-planned read asks for, and of which window.
pub(crate) enum ChartHistoryPlan {
    /// The read an older datahub gets: the last `preferences.chart_history_bars` bars, once.
    Classic,
    /// The window for the view, the interval to resample to, and the pieces the cache lacks.
    View { window: ChartHistoryWindow, resample_to: Option<&'static str>, missing: Vec<(i64, i64)> },
}

impl ChartHistory {
    /// Record what chart window `window` showed this frame (the chart's `ChartActions::view`). A view that
    /// differs from the last one restarts the debounce.
    pub fn observe_view(&mut self, window: egui::Id, view: Option<ChartViewSpan>, now_ms: i64) {
        let Some(view) = view else { return };
        match self.views.get_mut(&window) {
            Some((seen, _)) if *seen == view => {}
            _ => {
                self.views.insert(window, (view, now_ms));
            }
        }
    }

    /// Re-frame the chart of `key` (its seed reset, `follow.on_series_change()`) when it was just given a
    /// new window of history and now holds it in `bars`: the shell calls this before each chart draw.
    pub fn reframe(&mut self, key: &str, bars: &[ChartBar], follow: &mut ChartFollowLive) {
        if self.take_reframe(key, bars) {
            follow.on_series_change();
        }
    }

    /// Whether the chart of `key`, now holding `bars`, has just been given a new display it must be
    /// re-framed onto (its seed reset): once, on the first frame its bars ARE that display.
    pub(crate) fn take_reframe(&mut self, key: &str, bars: &[ChartBar]) -> bool {
        let mut cache = self.cache.lock();
        let held = (bars.first().map(|b| b.ot), bars.last().map(|b| b.ot), bars.len());
        let due = cache.landed.get(key).is_some_and(|&(a, b, n)| (Some(a), Some(b), n) == held);
        if due {
            cache.landed.remove(key);
        }
        due
    }

    /// Forget the session: a backend switch, whose store is another server's.
    pub fn clear(&mut self) {
        self.views.clear();
        self.shown.clear();
        self.cache = Arc::default();
    }

    /// The half the read thread fills.
    pub fn cache(&self) -> Arc<ChartHistoryCache> {
        Arc::clone(&self.cache)
    }

    /// What to read for chart window `id` on series `key` (a chart of `native` ms that holds bars or not),
    /// or `None` when nothing is due. Marks the read in flight.
    pub(crate) fn plan(
        &mut self,
        id: egui::Id,
        key: &str,
        native: i64,
        has_bars: bool,
        now_ms: i64,
    ) -> Option<ChartHistoryPlan> {
        let mut cache = self.cache.lock();
        if cache.resamples == Some(false) {
            return (!has_bars && cache.asked.insert(key.to_string()))
                .then_some(ChartHistoryPlan::Classic);
        }
        if cache.in_flight.contains(key) {
            return None;
        }
        let seen = self.views.get(&id);
        let (window, resample_to) = if !has_bars && !self.shown.contains_key(key) {
            (opening_window(native, seen.map(|(v, _)| v.width_px), now_ms), None)
        } else {
            let (view, since) = seen?;
            let span = view.timestamps?;
            if now_ms - since < VIEW_DEBOUNCE_MS {
                return None;
            }
            let (window, to) = view_window(native, span, view.width_px, now_ms);
            if self.shown.get(key).is_some_and(|s| displays(s, window.step_ms, span)) {
                return None;
            }
            (window, to)
        };
        let missing = cache.missing(key, window);
        self.shown.insert(key.to_string(), window);
        cache.in_flight.insert(key.to_string());
        Some(ChartHistoryPlan::View { window, resample_to, missing })
    }

    /// What to read for chart window `id` on series `key`, which the node PUBLISHES and whose
    /// oldest bar held opens at `first_open`: its own interval from the view's left edge to that
    /// bar, once the view has held still panned left of it, and never the same window twice
    /// running. Marks the read in flight.
    pub(crate) fn plan_under_live_tail(
        &mut self,
        id: egui::Id,
        key: &str,
        native: i64,
        first_open: i64,
        now_ms: i64,
    ) -> Option<ChartHistoryPlan> {
        let mut cache = self.cache.lock();
        if cache.in_flight.contains(key) {
            return None;
        }
        let (view, since) = self.views.get(&id)?;
        let (t0, t1) = view.timestamps?;
        let left = t0.min(t1);
        if now_ms - since < VIEW_DEBOUNCE_MS || left >= first_open - VIEW_SLACK_BARS * native {
            return None;
        }
        let window = ChartHistoryWindow::over(native, left, first_open - 1, now_ms);
        if self.shown.get(key) == Some(&window) {
            return None;
        }
        let missing = cache.missing(key, window);
        self.shown.insert(key.to_string(), window);
        cache.in_flight.insert(key.to_string());
        Some(ChartHistoryPlan::View { window, resample_to: None, missing })
    }
}

/// The session's cache of what the datahub answered, by series and bar width, shared with the read
/// thread; plus whether that datahub resamples, the reads in flight and the displays that landed.
#[derive(Default)]
pub struct ChartHistoryCache {
    state: Mutex<ChartHistoryCacheState>,
}

#[derive(Default)]
pub(crate) struct ChartHistoryCacheState {
    /// Whether the datahub serves `bar_resample`: `None` until a read asked it.
    pub(crate) resamples: Option<bool>,
    /// Keys read the way an older datahub is read, once per session.
    pub(crate) asked: HashSet<String>,
    pub(crate) in_flight: HashSet<String>,
    /// The display each chart was just given, `(first open, last open, bars)`, until it re-framed.
    pub(crate) landed: HashMap<String, (i64, i64, usize)>,
    held: HashMap<(String, i64), HeldBars>,
    clock: u64,
}

#[derive(Default)]
struct HeldBars {
    /// Ascending, merged, inclusive: the ranges the datahub answered.
    covered: Vec<(i64, i64)>,
    /// Ascending by open.
    bars: Vec<Bar>,
    used: u64,
}

impl ChartHistoryCache {
    pub(crate) fn lock(&self) -> MutexGuard<'_, ChartHistoryCacheState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl ChartHistoryCacheState {
    /// The pieces of `window` the cache does not hold for `key`, ascending.
    pub(crate) fn missing(&self, key: &str, window: ChartHistoryWindow) -> Vec<(i64, i64)> {
        let covered = self
            .held
            .get(&(key.to_string(), window.step_ms))
            .map(|h| h.covered.as_slice())
            .unwrap_or_default();
        let (mut out, mut from) = (Vec::new(), window.start);
        for &(a, b) in covered {
            if b < from {
                continue;
            }
            if a > window.end {
                break;
            }
            if a > from {
                out.push((from, a - 1));
            }
            from = b.saturating_add(1);
            if from > window.end {
                return out;
            }
        }
        out.push((from, window.end));
        out
    }

    /// Keep the datahub's answer `rows` for the piece `[lo, hi]` of `key` at `step_ms`: the piece's bars
    /// replace what was held there, and the piece is covered up to the bar holding `now_ms`, which is still
    /// forming and is asked again next time.
    pub(crate) fn hold(
        &mut self,
        key: &str,
        step_ms: i64,
        (lo, hi): (i64, i64),
        rows: Vec<Bar>,
        now_ms: i64,
    ) {
        self.clock += 1;
        let clock = self.clock;
        let held = self.held.entry((key.to_string(), step_ms)).or_default();
        held.used = clock;
        held.bars.retain(|b| b.timestamp < lo || b.timestamp > hi);
        held.bars.extend(rows.into_iter().filter(|b| (lo..=hi).contains(&b.timestamp)));
        held.bars.sort_by_key(|b| b.timestamp);
        let settled = hi.min(bar_open(now_ms, step_ms) - 1);
        if lo <= settled {
            held.covered.push((lo, settled));
            held.covered.sort_unstable();
            let mut merged: Vec<(i64, i64)> = Vec::with_capacity(held.covered.len());
            for (a, b) in held.covered.drain(..) {
                match merged.last_mut() {
                    Some(last) if a <= last.1.saturating_add(1) => last.1 = last.1.max(b),
                    _ => merged.push((a, b)),
                }
            }
            held.covered = merged;
        }
        self.evict(&(key.to_string(), step_ms));
    }

    /// The held bars of `key` at `window`'s width that open inside it.
    pub(crate) fn bars_in(&mut self, key: &str, window: ChartHistoryWindow) -> Vec<Bar> {
        self.clock += 1;
        let Some(held) = self.held.get_mut(&(key.to_string(), window.step_ms)) else {
            return Vec::new();
        };
        held.used = self.clock;
        let from = held.bars.partition_point(|b| b.timestamp < window.start);
        let to = held.bars.partition_point(|b| b.timestamp <= window.end);
        held.bars[from..to].to_vec()
    }

    /// Drop whole entries, the one used longest ago first, until the cache is under its ceiling; never
    /// `keep`, the one just written.
    fn evict(&mut self, keep: &(String, i64)) {
        loop {
            let total: usize = self.held.values().map(|h| h.bars.len()).sum();
            if total <= CACHE_BARS_CEILING {
                return;
            }
            let oldest = self
                .held
                .iter()
                .filter(|(k, _)| *k != keep)
                .min_by_key(|(_, h)| h.used)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    self.held.remove(&k);
                }
                None => return,
            }
        }
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;
